//! SHACL-SPARQL (SHACL §5–6): `sh:sparql` constraints and SPARQL-based constraint
//! components (`sh:ConstraintComponent` with ASK / SELECT validators).
//!
//! Pre-binding (`$this`, `$value`, `$currentShape`, `$shapesGraph` and component
//! parameters) is done by substituting constants for the variables in the planner
//! (`Planner::subst`), so blank node focus nodes work too. `$PATH` is replaced
//! textually with the SPARQL form of the shape's path before parsing.

use crate::nodeexpr::Expr;
use crate::path::PropertyPath;
use crate::shapes::{Constraint, G, ShapeId, Shapes};
use crate::validate::{Engine, Out, RPath};
use crate::vocab::{owl, sh};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::{Literal, NamedNode, Term};
use rustc_hash::FxHashSet;
use spargebra::Query;
use spargebra::algebra::GraphPattern;
use sparkles_core::id::{Id, Tag};
use sparkles_core::sparql::plan::{ActiveGraph, Planner};
use sparkles_core::sparql::table::Table;
use sparkles_core::sparql::{Ctx, depth, exec, parse_query};
use std::sync::Arc;

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
pub(crate) fn prefixes(g: &G<'_>, node: &Term) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    let mut stack = g.objects(node, sh::PREFIXES);
    while let Some(p) = stack.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        for d in g.objects(&p, sh::DECLARE) {
            if let (Some(pre), Some(ns)) = (g.object(&d, sh::PREFIX), g.object(&d, sh::NAMESPACE))
                && let (Some(pre), Some(ns)) = (literal_text(&pre), literal_text(&ns))
            {
                out.push((pre.to_string(), ns.to_string()));
            }
        }
        stack.extend(g.objects(&p, owl::IMPORTS));
    }
    out
}

fn messages(g: &G<'_>, node: &Term) -> Vec<Literal> {
    g.objects(node, sh::MESSAGE)
        .into_iter()
        .filter_map(|t| match t {
            Term::Literal(l) => Some(l),
            _ => None,
        })
        .collect()
}

/// Replace the `$PATH` placeholder (not `?PATH`, which is a normal variable) with a
/// SPARQL path. Only real tokens are replaced: the same text inside string literals,
/// IRIs or comments is left alone.
fn substitute_path(text: &str, path: Option<&PropertyPath>) -> String {
    let Some(p) = path else {
        return text.to_string();
    };
    let replacement = p.to_sparql();
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    // `text[copied..i]` is pending verbatim output; every delimiter below is ASCII, so
    // these byte offsets are always character boundaries
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                i = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |n| i + n)
            }
            q @ (b'"' | b'\'') => {
                let long = b[i..].starts_with(&[q, q, q]);
                let delim = if long { 3 } else { 1 };
                let mut j = i + delim;
                while j < b.len() {
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
            // an IRI reference; `<` followed by whitespace is the comparison operator
            b'<' => match b[i + 1..].iter().position(|&c| {
                c == b'>'
                    || c <= b' '
                    || matches!(c, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`')
            }) {
                Some(n) if b[i + 1 + n] == b'>' => i += n + 2,
                _ => i += 1,
            },
            b'$' if b[i + 1..].starts_with(b"PATH")
                && !text[i + 5..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_') =>
            {
                out.push_str(&text[copied..i]);
                out.push_str(&replacement);
                i += 5;
                copied = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
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

/// Check component validators even when no shape supplies their parameters, and
/// SPARQL functions even when the shapes do not call them.
pub(crate) fn check_gate_declarations(g: &G<'_>) -> Result<()> {
    // A validator may be used by a property shape. Its path cannot introduce a
    // graph or a dataset clause, so one placeholder suffices for this preflight.
    let path = PropertyPath::Predicate(NamedNode::new_unchecked("urn:sparkles:shacl:path"));
    for predicate in [sh::VALIDATOR, sh::NODE_VALIDATOR, sh::PROPERTY_VALIDATOR] {
        for triple in g.g.triples_for_predicate(predicate) {
            let component = Term::from(triple.subject.into_owned());
            if !g.is_instance(&component, sh::CONSTRAINT_COMPONENT) {
                continue;
            }
            let validator = triple.object.into_owned();
            for query in [sh::ASK, sh::SELECT]
                .into_iter()
                .flat_map(|p| g.objects(&validator, p))
            {
                let text = literal_text(&query)
                    .ok_or_else(|| anyhow!("the validator of {component} needs a query literal"))?;
                let prefixes = prefixes(g, &validator);
                check_gate_query(&parse_query(
                    &substitute_path(text, Some(&path)),
                    None,
                    &prefixes,
                )?)
                .with_context(|| format!("validator of {component}"))?;
            }
        }
    }
    for predicate in [sh::ASK, sh::SELECT] {
        for triple in g.g.triples_for_predicate(predicate) {
            let function = Term::from(triple.subject.into_owned());
            if !g.is_instance(&function, sh::SPARQL_FUNCTION) {
                continue;
            }
            let query = triple.object.into_owned();
            let text = literal_text(&query)
                .ok_or_else(|| anyhow!("the SPARQL function {function} needs a query literal"))?;
            check_gate_query(&parse_query(text, None, &prefixes(g, &function))?)
                .with_context(|| format!("SPARQL function {function}"))?;
        }
    }
    Ok(())
}

/// Check the actual queries after compilation, including substituted paths and
/// expressions nested in targets, constraints or custom function bodies.
pub(crate) fn check_gate_queries(shapes: &Shapes) -> Result<()> {
    for shape in &shapes.shapes {
        for constraint in &shape.constraints {
            let query = match constraint {
                Constraint::Sparql(c) => &c.parsed,
                Constraint::Component(c) => &c.parsed,
                _ => continue,
            };
            check_gate_query(query).with_context(|| format!("shape {}", shape.node))?;
        }
    }
    for (expr, node) in shapes.exprs.iter().zip(&shapes.expr_nodes) {
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
) -> Result<Option<SparqlConstraint>> {
    if g.objects(node, sh::DEACTIVATED)
        .iter()
        .any(|t| literal_text(t) == Some("true"))
    {
        return Ok(None);
    }
    let Some(sel) = g.object(node, sh::SELECT) else {
        bail!("sh:sparql constraint {node} has no sh:select");
    };
    let text = literal_text(&sel).ok_or_else(|| anyhow!("sh:select must be a literal"))?;
    let query = substitute_path(text, path);
    let prebound: Vec<String> = PREBOUND.iter().map(|s| s.to_string()).collect();
    let parsed = parse(&query, &prefixes(g, node), &prebound)?;
    if !matches!(parsed, Query::Select { .. }) {
        bail!("sh:select of {node} is not a SELECT query");
    }
    Ok(Some(SparqlConstraint {
        node: node.clone(),
        query,
        parsed,
        messages: messages(g, node),
    }))
}

fn local_name(iri: &str) -> &str {
    let i = iri.rfind(['#', '/', ':']).map_or(0, |i| i + 1);
    &iri[i..]
}

fn parse_validator(g: &G<'_>, node: &Term) -> Option<Validator> {
    let (ask, text) = match (g.object(node, sh::ASK), g.object(node, sh::SELECT)) {
        (Some(a), _) => (true, literal_text(&a)?.to_string()),
        (None, Some(s)) => (false, literal_text(&s)?.to_string()),
        _ => return None,
    };
    Some(Validator {
        ask,
        text,
        prefixes: prefixes(g, node),
        messages: messages(g, node),
    })
}

/// All SPARQL-based constraint components declared in the shapes graph.
pub(crate) fn parse_components(g: &G<'_>) -> Result<Vec<Arc<SparqlComponent>>> {
    let mut out = Vec::new();
    let mut cands: Vec<Term> = Vec::new();
    for t in g.g.triples_for_predicate(sh::PARAMETER) {
        let s = Term::from(t.subject.into_owned());
        if !cands.contains(&s) {
            cands.push(s);
        }
    }
    for c in cands {
        let Term::NamedNode(iri) = &c else { continue };
        if iri.as_str().starts_with(crate::vocab::SH_NS)
            || !g.is_instance(&c, sh::CONSTRAINT_COMPONENT)
        {
            continue;
        }
        let mut params = Vec::new();
        for p in g.objects(&c, sh::PARAMETER) {
            let Some(Term::NamedNode(path)) = g.object(&p, sh::PATH) else {
                bail!("parameter {p} of {c} has no IRI sh:path");
            };
            let optional = g
                .objects(&p, sh::OPTIONAL)
                .iter()
                .any(|t| literal_text(t) == Some("true"));
            params.push(Parameter {
                var: local_name(path.as_str()).to_string(),
                path,
                optional,
            });
        }
        let v = |p| g.object(&c, p).and_then(|n| parse_validator(g, &n));
        out.push(Arc::new(SparqlComponent {
            iri: iri.clone(),
            params,
            validator: v(sh::VALIDATOR),
            node_validator: v(sh::NODE_VALIDATOR),
            property_validator: v(sh::PROPERTY_VALIDATOR),
            messages: messages(g, &c),
        }));
    }
    Ok(out)
}

impl SparqlComponent {
    /// Instances of this component in shape `node` (one per combination of parameter
    /// values).
    pub(crate) fn instances(
        self: &Arc<Self>,
        g: &G<'_>,
        node: &Term,
        path: Option<&PropertyPath>,
    ) -> Result<Vec<ComponentConstraint>> {
        let mut combos: Vec<Vec<(String, Term)>> = vec![Vec::new()];
        for p in &self.params {
            let vals = g.objects(node, p.path.as_ref());
            if vals.is_empty() {
                if p.optional {
                    continue;
                }
                return Ok(Vec::new());
            }
            combos = combos
                .into_iter()
                .flat_map(|c| {
                    vals.iter().map(move |v| {
                        let mut c = c.clone();
                        c.push((p.var.clone(), v.clone()));
                        c
                    })
                })
                .collect();
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
        let text = substitute_path(&validator.text, path);
        let parsed = parse(&text, &validator.prefixes, &prebound)
            .with_context(|| format!("validator of {}", self.iri))?;
        let messages = if validator.messages.is_empty() {
            self.messages.clone()
        } else {
            validator.messages.clone()
        };
        Ok(combos
            .into_iter()
            .map(|bindings| ComponentConstraint {
                component: self.clone(),
                bindings,
                parsed: parsed.clone(),
                ask: validator.ask,
                messages: messages.clone(),
            })
            .collect())
    }
}

/// Replace `{$var}` / `{?var}` in message templates.
fn fill_template(msgs: &[Literal], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<Literal> {
    let re = regex::Regex::new(r"\{[\$\?]([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex");
    msgs.iter()
        .map(|m| {
            let s = re.replace_all(m.value(), |c: &regex::Captures<'_>| {
                lookup(&c[1]).unwrap_or_else(|| c[0].to_string())
            });
            match m.language() {
                Some(l) => Literal::new_language_tagged_literal_unchecked(s, l),
                None => Literal::new_simple_literal(s),
            }
        })
        .collect()
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
        let depth = depth::check_query(q).map_err(|e| anyhow!("{e}"))?;
        depth::with_stack(depth, || self.run_query_on(q, binds))
    }

    fn run_query_on(&self, q: &Query, binds: &[(&str, Term)]) -> Result<(Table, Arc<Ctx>)> {
        let mut ctx = Ctx::new(self.data.snap.clone());
        ctx.use_cache = false;
        ctx.allow_service = false;
        ctx.forbid_service = true;
        ctx.deadline = self.deadline;
        if let Some(m) = self.query_memory {
            ctx.mem_limit = ctx.mem_limit.min(m);
        }
        ctx.cancellation = self.cancellation.clone();
        if let Some(c) = &self.cancel {
            ctx.cancel = c.clone();
        }
        ctx.dataset.default = Some(self.data.sel.ids(&self.data.snap)?);
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
            planner
                .plan(&pattern, &ActiveGraph::Default, Vec::new())
                .map_err(|e| anyhow!("{e}"))?
        };
        // A shared context pulls an ASK, a LIMIT and an EXISTS only to the rows they need.
        let ctx = ctx.into_shared();
        let (table, _) = exec::execute(&ctx, &node).map_err(|e| anyhow!("{e}"))?;
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
    pub(crate) fn fresh_result_term(ctx: &Ctx, id: Id, tag: &str) -> Option<Term> {
        Some(fresh_blank_nodes(ctx, Self::result_term(ctx, id)?, tag))
    }

    pub(crate) fn result_term(ctx: &Ctx, id: Id) -> Option<Term> {
        if id.is_undef() {
            return None;
        }
        match id.tag() {
            Tag::Undef | Tag::Special => None,
            _ => ctx.term(id),
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
            let get = |c: Option<usize>| c.and_then(|c| Self::result_term(ctx, table.get(row, c)));
            if let Some(Term::Literal(l)) = get(c_fail)
                && l.value() == "true"
            {
                bail!(
                    "SHACL-SPARQL constraint of shape {} reported a failure",
                    shape.node
                );
            }
            let this = get(c_this).unwrap_or_else(|| focus_term.clone());
            let value = get(c_value).or_else(|| shape.path.is_none().then(|| focus_term.clone()));
            let path = if shape.path.is_some() {
                RPath::Shape
            } else {
                match get(c_path) {
                    Some(Term::NamedNode(p)) => RPath::Path(PropertyPath::Predicate(p)),
                    _ => RPath::None,
                }
            };
            let msgs = match get(c_msg) {
                Some(Term::Literal(l)) => vec![l],
                _ => fill_template(messages, &|v| {
                    if let Some((_, t)) = params.iter().find(|(n, _)| n == v) {
                        return Some(display(t));
                    }
                    let c = table.col_of(ctx.var(v))?;
                    Self::result_term(ctx, table.get(row, c)).map(|t| display(&t))
                }),
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
    use super::substitute_path;
    use crate::path::PropertyPath;
    use oxrdf::NamedNode;

    #[test]
    fn path_placeholder_is_replaced_only_as_a_token() {
        let p = PropertyPath::Predicate(NamedNode::new_unchecked("http://ex.org/p"));
        let text = r#"SELECT $this WHERE {
  $this $PATH ?v .   # $PATH in a comment
  FILTER(STRLEN("$PATH") = 5 && ?v != '$PATH' && ?v != """a "$PATH" b""")
  FILTER(?v != <http://ex.org/$PATH>) FILTER(?x<$PATH)
  ?v $PATHS ?w . ?v ?PATH ?u .
}"#;
        let out = substitute_path(text, Some(&p));
        assert_eq!(
            out,
            r#"SELECT $this WHERE {
  $this <http://ex.org/p> ?v .   # $PATH in a comment
  FILTER(STRLEN("$PATH") = 5 && ?v != '$PATH' && ?v != """a "$PATH" b""")
  FILTER(?v != <http://ex.org/$PATH>) FILTER(?x<<http://ex.org/p>)
  ?v $PATHS ?w . ?v ?PATH ?u .
}"#
        );
        assert_eq!(substitute_path(text, None), text);
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
            crate::validate::Engine::fresh_result_term(&ctx, table.get(0, c), "q9").unwrap()
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
            crate::validate::Engine::fresh_result_term(&ctx, table.get(0, c), "q9").unwrap()
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
}
