//! SHACL-SPARQL (SHACL §5–6): `sh:sparql` constraints and SPARQL-based constraint
//! components (`sh:ConstraintComponent` with ASK / SELECT validators).
//!
//! Pre-binding (`$this`, `$value`, `$currentShape`, `$shapesGraph` and component
//! parameters) is done by substituting constants for the variables in the planner
//! (`Planner::subst`), so blank node focus nodes work too. `$PATH` is replaced
//! textually with the SPARQL form of the shape's path before parsing.

use crate::nodeexpr::Expr;
use crate::path::{PropertyPath, append_checked};
use crate::shapes::{Constraint, G, ShapeId, Shapes};
use crate::validate::{Engine, Out, RPath};
use crate::vocab::{owl, sh};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, NamedNodeRef, Term};
use rustc_hash::FxHashSet;
use spargebra::Query;
use spargebra::algebra::GraphPattern;
use sparkles_core::id::{Id, Tag};
use sparkles_core::sparql::plan::{ActiveGraph, Planner};
use sparkles_core::sparql::table::Table;
use sparkles_core::sparql::{Ctx, depth, exec, parse_query};
use std::sync::Arc;
use std::time::Instant;

/// Variables that may be pre-bound (and so must not be assigned by the query).
const PREBOUND: &[&str] = &["this", "value", "currentShape", "shapesGraph"];

/// An `sh:sparql` constraint.
#[derive(Clone, Debug)]
pub struct SparqlConstraint {
    /// the constraint node (`sh:sourceConstraint`)
    pub node: Term,
    /// query text after `$PATH` substitution
    pub query: String,
    pub(crate) parsed: Query,
    pub messages: Vec<Literal>,
}

/// A parameter of a SPARQL-based constraint component.
#[derive(Clone, Debug)]
pub struct Parameter {
    pub path: NamedNode,
    /// SPARQL variable name (local name of the path)
    pub var: String,
    pub optional: bool,
}

/// An ASK or SELECT validator.
#[derive(Clone, Debug)]
pub struct Validator {
    pub ask: bool,
    pub text: String,
    pub prefixes: Vec<(String, String)>,
    pub messages: Vec<Literal>,
}

/// A SPARQL-based constraint component declaration.
#[derive(Clone, Debug)]
pub struct SparqlComponent {
    pub iri: NamedNode,
    pub params: Vec<Parameter>,
    pub validator: Option<Validator>,
    pub node_validator: Option<Validator>,
    pub property_validator: Option<Validator>,
    pub messages: Vec<Literal>,
}

/// An instance of a SPARQL-based component in a shape.
#[derive(Clone, Debug)]
pub struct ComponentConstraint {
    pub component: Arc<SparqlComponent>,
    /// parameter variable → value
    pub bindings: Vec<(String, Term)>,
    pub(crate) parsed: Query,
    pub(crate) ask: bool,
    pub(crate) messages: Vec<Literal>,
}

fn literal_text(t: &Term) -> Option<&str> {
    match t {
        Term::Literal(l) => Some(l.value()),
        _ => None,
    }
}

/// Prefix declarations reachable from the `sh:prefixes` values of `node`
/// (following `owl:imports`).
pub(crate) fn prefixes(
    g: &G<'_>,
    node: &Term,
    deadline: Option<Instant>,
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    let mut stack = g.objects_until(node, sh::PREFIXES, deadline)?;
    while let Some(p) = stack.pop() {
        crate::data::check_deadline(deadline)?;
        if !seen.insert(p.clone()) {
            continue;
        }
        for d in g.values_until(&p, sh::DECLARE, deadline) {
            let d = d?;
            if let (Some(pre), Some(ns)) = (g.object(&d, sh::PREFIX), g.object(&d, sh::NAMESPACE))
                && let (Some(pre), Some(ns)) = (literal_text(&pre), literal_text(&ns))
            {
                out.push((pre.to_string(), ns.to_string()));
            }
        }
        for import in g.values_until(&p, owl::IMPORTS, deadline) {
            stack.push(import?);
        }
    }
    Ok(out)
}

/// Whether `c` can continue a SPARQL variable name: `PN_CHARS_U`, a digit, `#x00B7`,
/// `[#x0300-#x036F]` or `[#x203F-#x2040]` of the SPARQL 1.1 grammar.
pub(crate) fn varname_char(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '\u{B7}'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{37D}'
        | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{203F}'..='\u{2040}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// Replace the `$PATH` placeholder (not `?PATH`, which is a normal variable) with a
/// SPARQL path. Only real tokens are replaced: the same text inside string literals,
/// IRIs or comments is left alone.
fn substitute_path(
    text: &str,
    path: Option<&PropertyPath>,
    deadline: Option<Instant>,
) -> Result<String> {
    let check = || crate::data::check_deadline(deadline);
    check()?;
    let mut out = String::new();
    let Some(path) = path else {
        append_checked(&mut out, text, &check)?;
        return Ok(out);
    };
    let mut replacement: Option<String> = None;
    let b = text.as_bytes();
    let mut next_check = 4096;
    let mut poll = |position: usize| {
        if position >= next_check {
            check()?;
            next_check = position.saturating_add(4096);
        }
        Ok::<(), sparkles_core::Error>(())
    };
    // The ASCII delimiters below keep each pending output boundary on a UTF-8 boundary.
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        poll(i)?;
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    poll(i)?;
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                let long = b[i..].starts_with(&[q, q, q]);
                let delim = if long { 3 } else { 1 };
                let mut j = i + delim;
                while j < b.len() {
                    poll(j)?;
                    if b[j] == b'\\' {
                        j += 2;
                    } else if b[j..].starts_with(&[q, q, q][..delim]) {
                        j += delim;
                        break;
                    } else if !long && b[j] == b'\n' {
                        break;
                    } else {
                        j += 1;
                    }
                }
                i = j.min(b.len());
            }
            // An IRI reference. A '<' followed by whitespace is the comparison operator.
            b'<' => {
                let mut j = i + 1;
                while let Some(&c) = b.get(j) {
                    poll(j)?;
                    if c == b'>'
                        || c <= b' '
                        || matches!(c, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`')
                    {
                        break;
                    }
                    j += 1;
                }
                i = if b.get(j) == Some(&b'>') {
                    j + 1
                } else {
                    i + 1
                };
            }
            b'$' if b[i + 1..].starts_with(b"PATH")
                && !text[i + 5..].chars().next().is_some_and(varname_char) =>
            {
                let replacement = match &replacement {
                    Some(rendered) => rendered,
                    None => replacement.insert(path.to_sparql_checked(check)?),
                };
                append_checked(&mut out, &text[copied..i], &check)?;
                append_checked(&mut out, replacement, &check)?;
                i += 5;
                copied = i;
            }
            _ => i += 1,
        }
    }
    append_checked(&mut out, &text[copied..], &check)?;
    check()?;
    Ok(out)
}

/// SHACL-SPARQL's additional constraints on prebound variables and operations.
fn check_pattern(root: &GraphPattern, prebound: Option<&[String]>) -> Result<()> {
    use GraphPattern as GP;
    let assigned = |v: &oxrdf::Variable| -> Result<()> {
        if prebound.is_some_and(|vars| vars.iter().any(|p| p == v.as_str())) {
            bail!(
                "the pre-bound variable ?{} must not be assigned with AS",
                v.as_str()
            );
        }
        Ok(())
    };
    sparkles_core::sparql::visit_graph_patterns(root, |pattern| {
        match pattern {
            GP::Service { .. } => bail!("SERVICE is not allowed in SHACL-SPARQL queries"),
            GP::Minus { .. } if prebound.is_some() => {
                bail!("MINUS is not allowed in SHACL-SPARQL queries")
            }
            GP::Values { variables, .. } => {
                let Some(vars) = prebound else {
                    return Ok(());
                };
                if let Some(v) = variables
                    .iter()
                    .find(|v| vars.iter().any(|p| p == v.as_str()))
                {
                    bail!(
                        "VALUES must not mention the pre-bound variable ?{}",
                        v.as_str()
                    );
                }
                bail!("VALUES is not allowed in SHACL-SPARQL queries");
            }
            GP::Extend { variable, .. } | GP::Assign { variable, .. } => {
                assigned(variable)?;
            }
            GP::Unfold {
                variable, second, ..
            } => {
                assigned(variable)?;
                if let Some(second) = second {
                    assigned(second)?;
                }
            }
            GP::Group { aggregates, .. } => {
                for (variable, _) in aggregates {
                    assigned(variable)?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

/// Node expressions use ordinary SPARQL forms, but never outbound SERVICE.
pub(crate) fn check_no_service(query: &Query) -> Result<()> {
    check_pattern(query_pattern(query), None)
}

fn query_pattern(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

fn check_gate_query(query: &Query) -> Result<()> {
    let dataset = match query {
        Query::Select { dataset, .. }
        | Query::Ask { dataset, .. }
        | Query::Construct { dataset, .. }
        | Query::Describe { dataset, .. } => dataset,
    };
    if dataset.is_some() {
        bail!("FROM and FROM NAMED are not allowed in the gate's SHACL profile");
    }
    sparkles_core::sparql::visit_graph_patterns(query_pattern(query), |pattern| match pattern {
        GraphPattern::Graph { .. } => {
            bail!("GRAPH is not allowed in the gate's SHACL profile");
        }
        GraphPattern::Service { .. } => {
            bail!("SERVICE is not allowed in SHACL-SPARQL queries");
        }
        _ => Ok(()),
    })
}

fn check_gate_validator(
    text: &str,
    prefixes: &[(String, String)],
    predicate: NamedNodeRef<'_>,
    property_path: &PropertyPath,
    deadline: Option<Instant>,
) -> Result<()> {
    crate::data::check_deadline(deadline)?;
    let parse = |text: &str| -> Result<Query> {
        crate::data::check_deadline(deadline)?;
        let query = parse_query(text, None, prefixes);
        crate::data::check_deadline(deadline)?;
        query.map_err(Into::into)
    };
    if predicate == sh::NODE_VALIDATOR {
        return check_gate_query(&parse(text)?);
    }
    let property = substitute_path(text, Some(property_path), deadline)?;
    if predicate == sh::PROPERTY_VALIDATOR {
        return check_gate_query(&parse(&property)?);
    }
    if property == text {
        return check_gate_query(&parse(text)?);
    }
    // A generic fallback can be used by either kind of shape. Check every
    // available syntax here. The instance still has to parse in its actual context.
    let node = parse(text);
    crate::data::check_deadline(deadline)?;
    let property = parse(&property);
    crate::data::check_deadline(deadline)?;
    match (node, property) {
        (Ok(node), Ok(property)) => {
            check_gate_query(&node)?;
            check_gate_query(&property)
        }
        (Ok(query), Err(_)) | (Err(_), Ok(query)) => check_gate_query(&query),
        (Err(error), Err(_)) => Err(error),
    }
}

/// Check component validators even when no shape supplies their parameters, and
/// SPARQL functions even when the shapes do not call them.
pub(crate) fn check_gate_declarations(g: &G<'_>, deadline: Option<Instant>) -> Result<()> {
    // A validator may be used by a property shape. Its path cannot introduce a
    // graph or a dataset clause, so one placeholder suffices for this preflight.
    let path = PropertyPath::Predicate(NamedNode::new_unchecked("urn:sparkles:shacl:path"));
    for predicate in [sh::VALIDATOR, sh::NODE_VALIDATOR, sh::PROPERTY_VALIDATOR] {
        for triple in g.g.triples_for_predicate(predicate) {
            crate::data::check_deadline(deadline)?;
            let component = Term::from(triple.subject.into_owned());
            if !g.is_instance_until(&component, sh::CONSTRAINT_COMPONENT, deadline)? {
                continue;
            }
            let validator = triple.object.into_owned();
            for p in [sh::ASK, sh::SELECT] {
                for query in g.values_until(&validator, p, deadline) {
                    let query = query?;
                    let text = literal_text(&query).ok_or_else(|| {
                        anyhow!("the validator of {component} needs a query literal")
                    })?;
                    let prefixes = prefixes(g, &validator, deadline)?;
                    check_gate_validator(text, &prefixes, predicate, &path, deadline)
                        .with_context(|| format!("validator of {component}"))?;
                }
            }
        }
    }
    for predicate in [sh::ASK, sh::SELECT] {
        for triple in g.g.triples_for_predicate(predicate) {
            crate::data::check_deadline(deadline)?;
            let function = Term::from(triple.subject.into_owned());
            if !g.is_instance_until(&function, sh::SPARQL_FUNCTION, deadline)? {
                continue;
            }
            let query = triple.object.into_owned();
            let text = literal_text(&query)
                .ok_or_else(|| anyhow!("the SPARQL function {function} needs a query literal"))?;
            check_gate_query(&parse_query(
                text,
                None,
                &prefixes(g, &function, deadline)?,
            )?)
            .with_context(|| format!("SPARQL function {function}"))?;
        }
    }
    Ok(())
}

/// Check the actual queries after compilation, including substituted paths and
/// expressions nested in targets, constraints or custom function bodies.
pub(crate) fn check_gate_queries(shapes: &Shapes, deadline: Option<Instant>) -> Result<()> {
    for shape in &shapes.shapes {
        for constraint in &shape.constraints {
            crate::data::check_deadline(deadline)?;
            let query = match constraint {
                Constraint::Sparql(c) => &c.parsed,
                Constraint::Component(c) => &c.parsed,
                _ => continue,
            };
            check_gate_query(query).with_context(|| format!("shape {}", shape.node))?;
        }
    }
    for (expr, node) in shapes.exprs.iter().zip(&shapes.expr_nodes) {
        crate::data::check_deadline(deadline)?;
        let query = match expr {
            Expr::Select(q) | Expr::SparqlExpr(q) => &q.query,
            Expr::SparqlFn { query, .. } | Expr::SparqlCall { query, .. } => query.as_ref(),
            _ => continue,
        };
        check_gate_query(query).with_context(|| format!("node expression {node}"))?;
    }
    Ok(())
}

pub(crate) fn parse(
    text: &str,
    prefixes: &[(String, String)],
    prebound: &[String],
) -> Result<Query> {
    let q = parse_query(text, None, prefixes)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("invalid SHACL-SPARQL query:\n{text}"))?;
    check_pattern(query_pattern(&q), Some(prebound))?;
    Ok(q)
}

/// Parse an `sh:sparql` value (`None` if deactivated).
pub(crate) fn parse_sparql_constraint(
    g: &G<'_>,
    node: &Term,
    path: Option<&PropertyPath>,
    gate: bool,
    deadline: Option<Instant>,
) -> Result<Option<SparqlConstraint>> {
    let flags = g.objects_until(node, sh::DEACTIVATED, deadline)?;
    let deactivated = if gate {
        match flags.as_slice() {
            [] => false,
            [Term::Literal(l)]
                if l.datatype() == xsd::BOOLEAN
                    && matches!(l.value(), "true" | "false" | "1" | "0") =>
            {
                l.value() == "true"
            }
            _ => bail!("sh:deactivated must be a single xsd:boolean literal on {node}"),
        }
    } else {
        flags.iter().any(|t| literal_text(t) == Some("true"))
    };
    if deactivated {
        return Ok(None);
    }
    let Some(sel) = g.object(node, sh::SELECT) else {
        bail!("sh:sparql constraint {node} has no sh:select");
    };
    let text = literal_text(&sel).ok_or_else(|| anyhow!("sh:select must be a literal"))?;
    let query = substitute_path(text, path, deadline)?;
    let prebound: Vec<String> = PREBOUND.iter().map(|s| s.to_string()).collect();
    let prefixes = prefixes(g, node, deadline)?;
    crate::data::check_deadline(deadline)?;
    let parsed = parse(&query, &prefixes, &prebound);
    crate::data::check_deadline(deadline)?;
    let parsed = parsed?;
    if !matches!(parsed, Query::Select { .. }) {
        bail!("sh:select of {node} is not a SELECT query");
    }
    Ok(Some(SparqlConstraint {
        node: node.clone(),
        query,
        parsed,
        messages: g.literals_until(node, sh::MESSAGE, deadline)?,
    }))
}

fn local_name(iri: &str) -> &str {
    let i = iri.rfind(['#', '/', ':']).map_or(0, |i| i + 1);
    &iri[i..]
}

fn parse_validator(g: &G<'_>, node: &Term, deadline: Option<Instant>) -> Result<Option<Validator>> {
    let (ask, query) = match (g.object(node, sh::ASK), g.object(node, sh::SELECT)) {
        (Some(a), _) => (true, a),
        (None, Some(s)) => (false, s),
        _ => return Ok(None),
    };
    let Some(text) = literal_text(&query) else {
        return Ok(None);
    };
    Ok(Some(Validator {
        ask,
        text: text.to_owned(),
        prefixes: prefixes(g, node, deadline)?,
        messages: g.literals_until(node, sh::MESSAGE, deadline)?,
    }))
}

/// All SPARQL-based constraint components declared in the shapes graph.
pub(crate) fn parse_components(
    g: &G<'_>,
    deadline: Option<Instant>,
) -> Result<Vec<Arc<SparqlComponent>>> {
    let mut out = Vec::new();
    let mut cands: Vec<Term> = Vec::new();
    let mut seen = FxHashSet::default();
    for t in g.g.triples_for_predicate(sh::PARAMETER) {
        crate::data::check_deadline(deadline)?;
        if seen.insert(t.subject) {
            cands.push(Term::from(t.subject.into_owned()));
        }
    }
    for c in cands {
        crate::data::check_deadline(deadline)?;
        let Term::NamedNode(iri) = &c else { continue };
        if iri.as_str().starts_with(crate::vocab::SH_NS)
            || !g.is_instance_until(&c, sh::CONSTRAINT_COMPONENT, deadline)?
        {
            continue;
        }
        let mut params = Vec::new();
        for p in g.values_until(&c, sh::PARAMETER, deadline) {
            let p = p?;
            let Some(Term::NamedNode(path)) = g.object(&p, sh::PATH) else {
                bail!("parameter {p} of {c} has no IRI sh:path");
            };
            let mut optional = false;
            for value in g.values_until(&p, sh::OPTIONAL, deadline) {
                if literal_text(&value?) == Some("true") {
                    optional = true;
                    break;
                }
            }
            params.push(Parameter {
                var: local_name(path.as_str()).to_string(),
                path,
                optional,
            });
        }
        let validator = |p| match g.object(&c, p) {
            Some(node) => parse_validator(g, &node, deadline),
            None => Ok(None),
        };
        out.push(Arc::new(SparqlComponent {
            iri: iri.clone(),
            params,
            validator: validator(sh::VALIDATOR)?,
            node_validator: validator(sh::NODE_VALIDATOR)?,
            property_validator: validator(sh::PROPERTY_VALIDATOR)?,
            messages: g.literals_until(&c, sh::MESSAGE, deadline)?,
        }));
    }
    Ok(out)
}

#[cfg(test)]
thread_local! {
    static EMBEDDED_PLANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_embedded_plans() -> usize {
    EMBEDDED_PLANS.replace(0)
}

#[cfg(test)]
thread_local! {
    static INSTANCE_CONVERSIONS_LEFT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn observe_instance_conversion() {
    INSTANCE_CONVERSIONS_LEFT.with(|left| match left.get() {
        Some(0) => crate::data::set_deadline_checks_left(Some(0)),
        Some(n) => left.set(Some(n - 1)),
        None => {}
    });
}

impl SparqlComponent {
    /// Instances of this component in shape `node` (one per combination of parameter
    /// values).
    pub(crate) fn instances(
        self: &Arc<Self>,
        g: &G<'_>,
        node: &Term,
        path: Option<&PropertyPath>,
        deadline: Option<Instant>,
    ) -> Result<Vec<ComponentConstraint>> {
        let mut combos: Vec<Vec<(String, Term)>> = vec![Vec::new()];
        for p in &self.params {
            crate::data::check_deadline(deadline)?;
            let vals = g.objects_until(node, p.path.as_ref(), deadline)?;
            if vals.is_empty() {
                if p.optional {
                    continue;
                }
                return Ok(Vec::new());
            }
            let mut next = Vec::new();
            for combo in combos {
                for val in &vals {
                    if next.len() % 256 == 0 {
                        crate::data::check_deadline(deadline)?;
                    }
                    let mut combo = combo.clone();
                    combo.push((p.var.clone(), val.clone()));
                    next.push(combo);
                }
            }
            combos = next;
        }
        let validator = if path.is_some() {
            self.property_validator.as_ref().or(self.validator.as_ref())
        } else {
            self.node_validator.as_ref().or(self.validator.as_ref())
        };
        let Some(validator) = validator else {
            return Ok(Vec::new());
        };
        let mut prebound: Vec<String> = PREBOUND.iter().map(|s| s.to_string()).collect();
        prebound.extend(self.params.iter().map(|p| p.var.clone()));
        crate::data::check_deadline(deadline)?;
        let text = substitute_path(&validator.text, path, deadline)?;
        let parsed = parse(&text, &validator.prefixes, &prebound);
        crate::data::check_deadline(deadline)?;
        let parsed = parsed.with_context(|| format!("validator of {}", self.iri))?;
        let messages = if validator.messages.is_empty() {
            &self.messages
        } else {
            &validator.messages
        };
        let mut instances = Vec::new();
        for bindings in combos {
            #[cfg(test)]
            observe_instance_conversion();
            crate::data::check_deadline(deadline)?;
            let mut instance_messages = Vec::new();
            for message in messages {
                crate::data::check_deadline(deadline)?;
                instance_messages.push(message.clone());
            }
            instances.push(ComponentConstraint {
                component: self.clone(),
                bindings,
                parsed: parsed.clone(),
                ask: validator.ask,
                messages: instance_messages,
            });
        }
        Ok(instances)
    }
}

/// Replace `{$var}` / `{?var}` in message templates, propagating fallible lookups.
fn try_fill_template(
    msgs: &[Literal],
    lookup: &dyn Fn(&str) -> Result<Option<String>>,
) -> Result<Vec<Literal>> {
    let re = regex::Regex::new(r"\{[\$\?]([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex");
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let text = m.value();
        let mut rendered = String::with_capacity(text.len());
        let mut last = 0;
        for cap in re.captures_iter(text) {
            let full = cap.get(0).expect("full match");
            rendered.push_str(&text[last..full.start()]);
            match lookup(&cap[1])? {
                Some(val) => rendered.push_str(&val),
                None => rendered.push_str(full.as_str()),
            }
            last = full.end();
        }
        rendered.push_str(&text[last..]);
        out.push(match m.language() {
            Some(l) => Literal::new_language_tagged_literal_unchecked(rendered, l),
            None => Literal::new_simple_literal(rendered),
        });
    }
    Ok(out)
}

/// Replace `{$var}` / `{?var}` in message templates.
fn fill_template(msgs: &[Literal], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<Literal> {
    try_fill_template(msgs, &|v| Ok(lookup(v))).expect("infallible template lookup")
}

fn display(t: &Term) -> String {
    match t {
        Term::Literal(l) => l.value().to_string(),
        Term::NamedNode(n) => n.as_str().to_string(),
        t => t.to_string(),
    }
}

impl Engine<'_> {
    /// Execute a parsed query with pre-bound variables.
    pub(crate) fn run_query(&self, q: &Query, binds: &[(&str, Term)]) -> Result<(Table, Arc<Ctx>)> {
        let depth = depth::check_query(q)?;
        depth::with_stack(depth, || self.run_query_on(q, binds))
    }

    fn run_query_on(&self, q: &Query, binds: &[(&str, Term)]) -> Result<(Table, Arc<Ctx>)> {
        let ctx = if let Some(opts) = &self.query_limits {
            sparkles_core::sparql::validation_query_context(
                self.data,
                sparkles_core::sparql::QueryOptions {
                    deadline: self.controls.deadline,
                    cancel: self.controls.cancel.clone(),
                    cancellation: self.controls.cancellation.clone(),
                    max_rows: opts.max_rows,
                    max_memory_bytes: self
                        .query_memory
                        .into_iter()
                        .chain(opts.max_memory_bytes)
                        .min(),
                    max_rows_produced: opts.max_rows_produced,
                    work: opts.work.clone(),
                    ..Default::default()
                },
            )?
        } else {
            let mut ctx = Ctx::new(self.data.snap.clone());
            ctx.use_cache = false;
            ctx.allow_service = false;
            ctx.forbid_service = true;
            ctx.deadline = self.controls.deadline;
            ctx.cancellation = self.controls.cancellation.clone();
            if let Some(m) = self.query_memory {
                ctx.mem_limit = ctx.mem_limit.min(m);
            }
            if let Some(c) = &self.controls.cancel {
                ctx.cancel = c.clone();
            }
            ctx.dataset.default = Some(self.data.sel.ids(&self.data.snap)?);
            ctx
        };
        let pattern = match q {
            Query::Ask { pattern, .. } => GraphPattern::Slice {
                inner: Box::new(pattern.clone()),
                start: 0,
                length: Some(1),
            },
            q => query_pattern(q).clone(),
        };
        let node = {
            let mut planner = Planner::new(&ctx);
            for (name, t) in binds {
                let id = match t {
                    Term::NamedNode(g) if *name == "shapesGraph" => ctx.graph_id(g.as_str()),
                    t => ctx.intern_outside_term(t),
                };
                planner.subst.insert(ctx.var(name), id);
            }
            planner.plan(&pattern, &ActiveGraph::Default, Vec::new())?
        };
        #[cfg(test)]
        EMBEDDED_PLANS.with(|plans| plans.set(plans.get() + 1));
        // A shared context pulls an ASK, a LIMIT and an EXISTS only to the rows they need.
        let ctx = ctx.into_shared();
        let (table, _) = exec::execute(&ctx, &node)?;
        Ok((table, ctx))
    }

    /// Standard pre-bindings for a shape and focus node.
    fn base_bindings(&self, si: ShapeId, focus: Id) -> Result<Vec<(&'static str, Term)>> {
        let shape = &self.shapes.shapes[si];
        let mut b = vec![
            ("this", self.term(focus)?),
            ("currentShape", shape.node.clone()),
        ];
        if self.shapes.binds_shapes_graph {
            // the shapes graph is a graph of the store
            let g = match &self.shapes.source_graph {
                Some(iri) => Term::NamedNode(NamedNode::new_unchecked(iri.clone())),
                None => Term::NamedNode(NamedNode::new_unchecked(
                    sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI,
                )),
            };
            b.push(("shapesGraph", g));
        }
        Ok(b)
    }

    /// The term of a query result whose blank nodes are new in each query: a blank node
    /// the query made, rather than one of the store or one it was given, gets a label
    /// that starts with `tag`, which the caller makes unique per query. Blank nodes inside
    /// triple terms are renamed the same way.
    pub(crate) fn fresh_result_term(ctx: &Ctx, id: Id, tag: &str) -> Result<Option<Term>> {
        Ok(Self::result_term(ctx, id)?.map(|t| fresh_blank_nodes(ctx, t, tag)))
    }

    pub(crate) fn result_term(ctx: &Ctx, id: Id) -> Result<Option<Term>> {
        if id.is_undef() {
            return Ok(None);
        }
        match id.tag() {
            Tag::Undef | Tag::Special => Ok(None),
            _ => Ok(ctx.term_checked(id)?),
        }
    }

    pub(crate) fn check_sparql(
        &self,
        si: ShapeId,
        c: &SparqlConstraint,
        focus: Id,
        out: &mut Out,
    ) -> Result<()> {
        let binds = self.base_bindings(si, focus)?;
        let (table, ctx) = self.run_query(&c.parsed, &binds)?;
        self.select_results(
            si,
            &table,
            &ctx,
            focus,
            &c.messages,
            Some(c.node.clone()),
            sh::SPARQL_CC.into_owned(),
            &[],
            out,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn select_results(
        &self,
        si: ShapeId,
        table: &Table,
        ctx: &Ctx,
        focus: Id,
        messages: &[Literal],
        source_constraint: Option<Term>,
        component: NamedNode,
        params: &[(String, Term)],
        out: &mut Out,
    ) -> Result<()> {
        if table.is_empty() {
            return Ok(());
        }
        let shape = &self.shapes.shapes[si];
        let col = |name: &str| table.col_of(ctx.var(name));
        let (c_value, c_path, c_msg, c_fail, c_this) = (
            col("value"),
            col("path"),
            col("message"),
            col("failure"),
            col("this"),
        );
        let focus_term = self.term(focus)?;
        for row in 0..table.len() {
            let get = |c: Option<usize>| -> Result<Option<Term>> {
                match c {
                    Some(c) => Self::result_term(ctx, table.get(row, c)),
                    None => Ok(None),
                }
            };
            if let Some(Term::Literal(l)) = get(c_fail)?
                && l.value() == "true"
            {
                bail!(
                    "SHACL-SPARQL constraint of shape {} reported a failure",
                    shape.node
                );
            }
            let this = get(c_this)?.unwrap_or_else(|| focus_term.clone());
            let value = get(c_value)?.or_else(|| shape.path.is_none().then(|| focus_term.clone()));
            let path = if shape.path.is_some() {
                RPath::Shape
            } else {
                match get(c_path)? {
                    Some(Term::NamedNode(p)) => RPath::Path(PropertyPath::Predicate(p)),
                    _ => RPath::None,
                }
            };
            let msgs = match get(c_msg)? {
                Some(Term::Literal(l)) => vec![l],
                _ => try_fill_template(messages, &|v| {
                    if let Some((_, t)) = params.iter().find(|(n, _)| n == v) {
                        return Ok(Some(display(t)));
                    }
                    let Some(c) = table.col_of(ctx.var(v)) else {
                        return Ok(None);
                    };
                    Ok(Self::result_term(ctx, table.get(row, c))?.map(|t| display(&t)))
                })?,
            };
            self.push(
                out,
                si,
                this,
                path,
                value,
                component.clone(),
                source_constraint.clone(),
                msgs,
                || format!("SPARQL SELECT constraint for {focus_term} returns a result"),
            )?;
            if out.stop() {
                return Ok(());
            }
        }
        Ok(())
    }

    pub(crate) fn check_component(
        &self,
        si: ShapeId,
        cc: &ComponentConstraint,
        focus: Id,
        values: &[Id],
        out: &mut Out,
    ) -> Result<()> {
        let mut binds = self.base_bindings(si, focus)?;
        for (n, t) in &cc.bindings {
            binds.push((n.as_str(), t.clone()));
        }
        let comp = cc.component.iri.clone();
        let param = |v: &str| {
            cc.bindings
                .iter()
                .find(|(n, _)| n == v)
                .map(|(_, t)| display(t))
        };
        if cc.ask {
            let shape = &self.shapes.shapes[si];
            for &v in values {
                let vt = self.term(v)?;
                let mut b = binds.clone();
                b.push(("value", vt.clone()));
                let (table, _) = self.run_query(&cc.parsed, &b)?;
                if table.is_empty() {
                    let msgs = fill_template(&cc.messages, &|name| {
                        if name == "value" {
                            return Some(display(&vt));
                        }
                        param(name)
                    });
                    let path = if shape.path.is_some() {
                        RPath::Shape
                    } else {
                        RPath::None
                    };
                    self.push(
                        out,
                        si,
                        self.term(focus)?,
                        path,
                        Some(vt.clone()),
                        comp.clone(),
                        None,
                        msgs,
                        || format!("SPARQL ASK constraint for {vt} returns false"),
                    )?;
                    if out.stop() {
                        return Ok(());
                    }
                }
            }
            Ok(())
        } else {
            let (table, ctx) = self.run_query(&cc.parsed, &binds)?;
            self.select_results(
                si,
                &table,
                &ctx,
                focus,
                &cc.messages,
                None,
                comp,
                &cc.bindings,
                out,
            )
        }
    }
}

/// `t` with the blank nodes of a query's result renamed ([`Engine::fresh_result_term`]).
fn fresh_blank_nodes(ctx: &Ctx, t: Term, tag: &str) -> Term {
    match t {
        Term::BlankNode(b) if sparkles_core::store::parse_bnode_label(b.as_str()).is_none() => {
            let label = ctx
                .outside_label(b.as_str())
                .unwrap_or_else(|| format!("{tag}x{}", b.as_str()));
            Term::BlankNode(oxrdf::BlankNode::new_unchecked(label))
        }
        Term::Triple(tr) => {
            let tr = *tr;
            let subject = match fresh_blank_nodes(ctx, tr.subject.into(), tag) {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => unreachable!("a subject stays a subject"),
            };
            let object = fresh_blank_nodes(ctx, tr.object, tag);
            Term::Triple(Box::new(oxrdf::Triple::new(subject, tr.predicate, object)))
        }
        t => t,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gate_property_constraint_polls_wide_sparql_path_rendering() {
        use crate::path::ObservePathRender;
        use crate::shapes::{Constraint, Shapes};
        use crate::vocab::SH_NS;
        use sparkles_core::io::RdfFormat;
        use std::time::{Duration, Instant};

        let list = vec!["<urn:p>"; 128].join(" ");
        let input = format!(
            "@prefix sh: <{SH_NS}> .
             <urn:S> sh:targetNode <urn:x> ; sh:path ({list}) ;
                 sh:sparql [ sh:select 'SELECT $this WHERE {{ $this $PATH ?o }}' ] ."
        );
        let deadline = Instant::now().checked_add(Duration::from_secs(60));
        let shapes = Shapes::parse_for_gate(&input, RdfFormat::Turtle, None, deadline).unwrap();
        assert!(
            shapes
                .shapes
                .iter()
                .any(|shape| shape.constraints.iter().any(|c| {
                    matches!(c, Constraint::Sparql(c) if c.query.matches("<urn:p>").count() == 128)
                }))
        );
        let observed = ObservePathRender::start(true);
        let result = Shapes::parse_for_gate(&input, RdfFormat::Turtle, None, deadline);
        let visited = observed.count();
        assert!(
            matches!(
                result.as_ref().err().and_then(|e| e.downcast_ref()),
                Some(sparkles_core::Error::Timeout)
            ) && (5..=6).contains(&visited),
            "wide SPARQL substitution rendered {visited} predicates before returning"
        );
    }

    #[test]
    fn wide_component_path_is_polled_and_reused_for_repeated_tokens() {
        use crate::path::ObservePathRender;
        use crate::shapes::{Constraint, Shapes};
        use crate::vocab::SH_NS;
        use sparkles_core::io::RdfFormat;

        let list = vec!["<urn:p>"; 128].join(" ");
        let input = format!(
            "@prefix sh: <{SH_NS}> .
             <urn:Component> a sh:ConstraintComponent ;
                 sh:parameter [ sh:path <urn:parameter> ] ;
                 sh:propertyValidator [
                     sh:select '''SELECT $this ?value WHERE {{
                         $this $PATH ?value . $this $PATH ?other
                     }}'''
                 ] .
             <urn:shape> sh:targetNode <urn:x> ; sh:path ({list}) ;
                 <urn:parameter> true ."
        );
        let normal = ObservePathRender::start(false);
        let shapes = Shapes::parse(&input, RdfFormat::Turtle, None).unwrap();
        assert!(shapes.shapes.iter().any(|shape| {
            shape
                .constraints
                .iter()
                .any(|c| matches!(c, Constraint::Component(_)))
        }));
        assert_eq!(normal.count(), 128);
        drop(normal);
        let observed = ObservePathRender::start(true);
        let result = Shapes::parse(&input, RdfFormat::Turtle, None);
        assert!(matches!(
            result.as_ref().err().and_then(|e| e.downcast_ref()),
            Some(sparkles_core::Error::Timeout)
        ));
        assert_eq!(observed.count(), 5);
    }

    #[test]
    fn sparql_without_a_real_path_token_never_renders_the_path() {
        use crate::path::ObservePathRender;
        let p = PropertyPath::Sequence(vec![
            PropertyPath::Predicate(NamedNode::new_unchecked(
                "urn:p"
            ),);
            1024
        ]);
        let text = "SELECT ?PATH ?value { BIND('$PATH' AS ?value) # $PATH\n FILTER(?value = <urn:$PATH> || $PATHsuffix = $PATHé || $PATH\u{0301} = $PATH\u{203F}) }";
        let observed = ObservePathRender::start(false);
        assert_eq!(substitute_path(text, Some(&p), None).unwrap(), text);
        assert_eq!(observed.count(), 0, "rendered a path with no actual token");
    }

    use super::substitute_path;
    use crate::path::PropertyPath;
    use oxrdf::NamedNode;

    #[test]
    fn gate_component_conversion_polls_after_expansion() {
        use super::{G, INSTANCE_CONVERSIONS_LEFT, Parameter, SparqlComponent, Validator};
        use oxrdf::{Graph, Triple};
        use sparkles_core::Error;
        use std::sync::Arc;

        let node = NamedNode::new_unchecked("urn:shape");
        let parameter = NamedNode::new_unchecked("urn:value");
        let mut graph = Graph::new();
        for value in [1, 2, 3] {
            graph.insert(&Triple::new(
                node.clone(),
                parameter.clone(),
                oxrdf::Literal::from(value),
            ));
        }
        let component = Arc::new(SparqlComponent {
            iri: NamedNode::new_unchecked("urn:component"),
            params: vec![Parameter {
                path: parameter,
                var: "value".into(),
                optional: false,
            }],
            validator: Some(Validator {
                ask: false,
                text: "SELECT $this WHERE { FILTER(?value = 1) }".into(),
                prefixes: Vec::new(),
                messages: Vec::new(),
            }),
            node_validator: None,
            property_validator: None,
            messages: vec![oxrdf::Literal::new_simple_literal("message")],
        });
        INSTANCE_CONVERSIONS_LEFT.set(Some(1));
        let result = component.instances(&G { g: &graph }, &node.into(), None, None);
        INSTANCE_CONVERSIONS_LEFT.set(None);
        crate::data::set_deadline_checks_left(None);
        assert!(matches!(
            result.err().and_then(|e| e.downcast::<Error>().ok()),
            Some(Error::Timeout)
        ));
    }

    #[test]
    fn path_placeholder_is_replaced_only_as_a_token() {
        let p = PropertyPath::Predicate(NamedNode::new_unchecked("http://ex.org/p"));
        let text = r#"SELECT $this WHERE {
  $this $PATH ?v .   # $PATH in a comment
  FILTER(STRLEN("$PATH") = 5 && ?v != '$PATH' && ?v != """a "$PATH" b""")
  FILTER(?v != <http://ex.org/$PATH>) FILTER(?x<$PATH)
  ?v $PATHS ?w . ?v ?PATH ?u .
}"#;
        let out = substitute_path(text, Some(&p), None).unwrap();
        assert_eq!(
            out,
            r#"SELECT $this WHERE {
  $this <http://ex.org/p> ?v .   # $PATH in a comment
  FILTER(STRLEN("$PATH") = 5 && ?v != '$PATH' && ?v != """a "$PATH" b""")
  FILTER(?v != <http://ex.org/$PATH>) FILTER(?x<<http://ex.org/p>)
  ?v $PATHS ?w . ?v ?PATH ?u .
}"#
        );
        assert_eq!(substitute_path(text, None, None).unwrap(), text);
    }

    #[test]
    fn authoring_consult_extension_algebra_cannot_hide_service() {
        use super::{check_pattern, query_pattern};
        use spargebra::algebra::{
            AggregateExpression as A, Expression as E, GraphPattern as G, OrderExpression as O,
        };
        let q = sparkles_core::sparql::parse_query(
            "SELECT ?s WHERE { SERVICE <http://127.0.0.1:1/never-contact> { ?s ?p ?o } }",
            None,
            &[],
        )
        .unwrap();
        let service = query_pattern(&q).clone();
        let empty = || Box::new(G::Bgp { patterns: vec![] });
        let exists = || E::Exists(Box::new(service.clone()));
        let var = || oxrdf::Variable::new_unchecked("x");
        let cases = [
            G::SemiJoin {
                left: empty(),
                right: Box::new(service.clone()),
            },
            G::AntiJoin {
                left: empty(),
                right: Box::new(service.clone()),
            },
            G::Assign {
                inner: empty(),
                variable: var(),
                expression: exists(),
            },
            G::Unfold {
                inner: empty(),
                variable: var(),
                second: None,
                expression: exists(),
            },
            G::Group {
                inner: empty(),
                variables: vec![],
                aggregates: vec![(
                    var(),
                    A::Fold {
                        expr: exists(),
                        value: None,
                        distinct: false,
                        order: vec![],
                    },
                )],
            },
            G::Group {
                inner: empty(),
                variables: vec![],
                aggregates: vec![(
                    var(),
                    A::Fold {
                        expr: E::Literal(1.into()),
                        value: Some(exists()),
                        distinct: false,
                        order: vec![],
                    },
                )],
            },
            G::Group {
                inner: empty(),
                variables: vec![],
                aggregates: vec![(
                    var(),
                    A::Fold {
                        expr: E::Literal(1.into()),
                        value: None,
                        distinct: false,
                        order: vec![O::Desc(exists())],
                    },
                )],
            },
        ];
        for pattern in cases {
            let error = check_pattern(&pattern, None).unwrap_err();
            assert!(
                error.to_string().contains("SERVICE is not allowed"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn authoring_consult_engine_context_always_disables_service() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let shapes = crate::Shapes::default();
        let (data, ids) =
            crate::data::DataGraph::new(store.snapshot(), None, &[], &[], &shapes).unwrap();
        let engine =
            crate::validate::Engine::new(&shapes, &data, ids, &crate::ValidateOptions::default())
                .unwrap();
        let query =
            sparkles_core::sparql::parse_query("SELECT ?x WHERE { BIND(1 AS ?x) }", None, &[])
                .unwrap();
        let (_, ctx) = engine.run_query(&query, &[]).unwrap();
        assert!(!ctx.allow_service);
        assert!(ctx.forbid_service);
    }

    #[test]
    fn a_query_result_keeps_the_blank_nodes_it_was_given_and_renames_its_own() {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let shapes = crate::Shapes::default();
        let (data, ids) =
            crate::data::DataGraph::new(store.snapshot(), None, &[], &[], &shapes).unwrap();
        let engine =
            crate::validate::Engine::new(&shapes, &data, ids, &crate::ValidateOptions::default())
                .unwrap();
        let query = sparkles_core::sparql::parse_query(
            "SELECT ?given ?made WHERE { BIND($a AS ?given) BIND(BNODE() AS ?made) }",
            None,
            &[],
        )
        .unwrap();
        let given = oxrdf::Term::BlankNode(oxrdf::BlankNode::new_unchecked("q3xq0"));
        let (table, ctx) = engine.run_query(&query, &[("a", given.clone())]).unwrap();
        let col = |name: &str| {
            (0..table.width())
                .find(|&c| ctx.var_name(table.vars[c]) == name)
                .unwrap()
        };
        let term = |c: usize| {
            crate::validate::Engine::fresh_result_term(&ctx, table.get(0, c), "q9")
                .unwrap()
                .unwrap()
        };
        assert_eq!(term(col("given")), given);
        let oxrdf::Term::BlankNode(made) = term(col("made")) else {
            panic!("not a blank node")
        };
        assert!(made.as_str().starts_with("q9x"), "{made}");

        // and so do the blank nodes inside triple terms
        let query = sparkles_core::sparql::parse_query(
            "SELECT ?given ?made WHERE { BIND($t AS ?given) \
             BIND(TRIPLE(BNODE(), <http://x/p>, 1) AS ?made) }",
            None,
            &[],
        )
        .unwrap();
        let triple = oxrdf::Term::Triple(Box::new(oxrdf::Triple::new(
            oxrdf::BlankNode::new_unchecked("q3xq0"),
            oxrdf::NamedNode::new_unchecked("http://x/p"),
            oxrdf::BlankNode::new_unchecked("q3xq1"),
        )));
        let (table, ctx) = engine.run_query(&query, &[("t", triple.clone())]).unwrap();
        let col = |name: &str| {
            (0..table.width())
                .find(|&c| ctx.var_name(table.vars[c]) == name)
                .unwrap()
        };
        let term = |c: usize| {
            crate::validate::Engine::fresh_result_term(&ctx, table.get(0, c), "q9")
                .unwrap()
                .unwrap()
        };
        assert_eq!(term(col("given")), triple);
        let oxrdf::Term::Triple(made) = term(col("made")) else {
            panic!("not a triple term")
        };
        let oxrdf::NamedOrBlankNode::BlankNode(b) = &made.subject else {
            panic!("not a blank node")
        };
        assert!(b.as_str().starts_with("q9x"), "{b}");
    }

    /// A store whose two predicates each hold `side` triples, so a pattern that reads
    /// both without a shared variable answers `side * side` rows.
    fn product_store(side: usize) -> sparkles_core::store::Store {
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let mut data = String::new();
        for i in 0..side {
            data.push_str(&format!("<urn:a:{i}> <urn:p> <urn:b:{i}> .\n"));
            data.push_str(&format!("<urn:c:{i}> <urn:q> <urn:d:{i}> .\n"));
        }
        store
            .load(&[sparkles_core::io::Source::from_bytes(
                data.into_bytes(),
                sparkles_core::io::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store
    }

    /// The rows the operators of `text` produce when the engine runs it as a SHACL-SPARQL
    /// query, with `$this` bound to `<urn:a:7>`.
    fn produced(store: &sparkles_core::store::Store, text: &str) -> (usize, u64) {
        let shapes = crate::Shapes::default();
        let (data, ids) =
            crate::data::DataGraph::new(store.snapshot(), None, &[], &[], &shapes).unwrap();
        let engine =
            crate::validate::Engine::new(&shapes, &data, ids, &crate::ValidateOptions::default())
                .unwrap();
        let query = sparkles_core::sparql::parse_query(text, None, &[]).unwrap();
        let this = oxrdf::Term::NamedNode(NamedNode::new_unchecked("urn:a:7"));
        let (table, ctx) = engine.run_query(&query, &[("this", this)]).unwrap();
        (table.len(), ctx.rows_produced())
    }

    // A SHACL-SPARQL ASK, a LIMIT and an EXISTS read their input until they have the rows
    // they need, as a query does (items 827 and 980). Each pattern below answers a million
    // rows, and the pulled prefix produces far fewer.
    const PULLED: u64 = 300_000;

    #[test]
    fn a_shacl_ask_reads_its_pattern_until_its_first_solution() {
        let store = product_store(1_000);
        let (rows, produced) = produced(&store, "ASK { ?a <urn:p> ?b . ?c <urn:q> ?d }");
        assert_eq!(rows, 1);
        assert!(produced < PULLED, "{produced}");
        let (_, produced) = produced_union(&store);
        assert!(produced < PULLED, "{produced}");
    }

    fn produced_union(store: &sparkles_core::store::Store) -> (usize, u64) {
        produced(
            store,
            "ASK { { ?a <urn:p> ?b . ?c <urn:q> ?d } UNION { ?a <urn:q> ?b . ?c <urn:p> ?d } }",
        )
    }

    #[test]
    fn a_shacl_limit_reads_its_pattern_until_its_limit() {
        let store = product_store(1_000);
        let (rows, produced) = produced(
            &store,
            "SELECT $this ?c WHERE { $this <urn:p> ?b . ?c <urn:q> ?d . ?e <urn:p> ?f } LIMIT 1",
        );
        assert_eq!(rows, 1);
        assert!(produced < PULLED, "{produced}");
    }

    #[test]
    fn a_shacl_exists_reads_its_pattern_until_its_first_solution() {
        let store = product_store(1_000);
        let (rows, produced) = produced(
            &store,
            "SELECT $this WHERE { FILTER EXISTS { $this <urn:p> ?b . ?c <urn:q> ?d . ?e <urn:p> ?f } }",
        );
        assert_eq!(rows, 1);
        assert!(produced < PULLED, "{produced}");
    }

    #[test]
    fn shacl_sparql_constraint_and_rule_propagate_result_decode_refusal() {
        use crate::vocab::SH_NS;
        use oxrdf::{Literal, Term};
        use sparkles_core::io::{RdfFormat, Source};

        let big = "s".repeat(16 * 1024);
        let data_ttl = format!(
            "@prefix ex: <http://example.org/> .\n\
             ex:s ex:big \"{big}\" ; ex:small \"ok\" .\n"
        );
        let store = sparkles_core::store::Store::in_memory(Default::default());
        store
            .load(&[Source::from_bytes(
                data_ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store.compact().unwrap();

        // 1. SHACL-SPARQL SELECT constraint projecting an oversized literal without decoding
        //    it during the query itself; result_term hits the budget first and must fail
        //    instead of substituting fallback focus node or unsubstituted template message.
        let constraint_shapes_ttl = format!(
            "@prefix sh: <{SH_NS}> .\n\
             @prefix ex: <http://example.org/> .\n\
             ex:Shape a sh:NodeShape ;\n\
               sh:targetNode ex:s ;\n\
               sh:sparql [\n\
                 sh:message \"bad {{$value}}\" ;\n\
                 sh:select \"PREFIX ex: <http://example.org/> SELECT $this ?value WHERE {{ $this ex:big ?value }}\" ;\n\
               ] .\n"
        );
        let constraint_shapes =
            crate::Shapes::parse(&constraint_shapes_ttl, RdfFormat::Turtle, None).unwrap();

        let ok_opts = crate::ValidateOptions {
            query_memory: Some(1 << 20),
            ..Default::default()
        };
        let report_ok = crate::validate(&store.snapshot(), &constraint_shapes, &ok_opts).unwrap();
        assert!(!report_ok.conforms);
        assert_eq!(report_ok.results.len(), 1);
        assert_eq!(
            report_ok.results[0].value,
            Some(Term::Literal(Literal::new_simple_literal(&big)))
        );
        assert!(
            report_ok.results[0]
                .messages
                .iter()
                .any(|m| m.value().starts_with("bad ssss")),
            "positive control substitutes $value into message"
        );

        let tight_opts = crate::ValidateOptions {
            query_memory: Some(32 * 1024),
            ..Default::default()
        };
        let err = crate::validate(&store.snapshot(), &constraint_shapes, &tight_opts).expect_err(
            "tight query_memory must fail result_term decode instead of substituting fallbacks",
        );
        assert!(
            matches!(
                err.downcast_ref::<sparkles_core::Error>(),
                Some(sparkles_core::Error::BudgetExceeded(b))
                    if matches!(b.kind, sparkles_core::BudgetKind::Memory | sparkles_core::BudgetKind::ServerMemory)
            ),
            "expected Memory BudgetExceeded, got {err:#}"
        );

        // 2. SHACL-SPARQL rule template emitting an oversized literal; fresh_result_term
        //    must propagate the decode refusal instead of silently skipping the triple.
        let rule_shapes_ttl = format!(
            "@prefix sh: <{SH_NS}> .\n\
             @prefix ex: <http://example.org/> .\n\
             ex:RuleShape a sh:NodeShape ;\n\
               sh:targetNode ex:s ;\n\
               sh:rule [\n\
                 a sh:SPARQLRule ;\n\
                 sh:construct \"PREFIX ex: <http://example.org/> CONSTRUCT {{ $this ex:copy ?v }} WHERE {{ $this ex:big ?v }}\" ;\n\
               ] .\n"
        );
        let rule_shapes = crate::Shapes::parse(&rule_shapes_ttl, RdfFormat::Turtle, None).unwrap();
        let [rule] = rule_shapes.rules().unwrap() else {
            panic!("expected one active SPARQL rule");
        };
        let run_rule = |opts: &crate::ValidateOptions, emitted: &mut Vec<_>| {
            crate::rules::RuleData::new(store.snapshot(), &rule_shapes, None, &[], opts)?.run(
                &rule_shapes,
                rule,
                &mut |triple| {
                    emitted.push(triple);
                    Ok(())
                },
            )
        };
        let mut inferred = Vec::new();
        run_rule(&ok_opts, &mut inferred).unwrap();
        assert_eq!(inferred.len(), 1);

        let mut inferred = Vec::new();
        let rule_err = run_rule(&tight_opts, &mut inferred)
            .expect_err("tight query memory must refuse decoding the rule result");
        assert!(inferred.is_empty());
        assert!(
            matches!(
                rule_err.downcast_ref::<sparkles_core::Error>(),
                Some(sparkles_core::Error::BudgetExceeded(b))
                    if matches!(b.kind, sparkles_core::BudgetKind::Memory | sparkles_core::BudgetKind::ServerMemory)
            ),
            "expected Memory BudgetExceeded from SHACL rule, got {rule_err:#}"
        );
    }
}
