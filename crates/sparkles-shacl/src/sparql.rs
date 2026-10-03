//! SHACL-SPARQL (SHACL §5–6): `sh:sparql` constraints and SPARQL-based constraint
//! components (`sh:ConstraintComponent` with ASK / SELECT validators).
//!
//! Pre-binding (`$this`, `$value`, `$currentShape`, `$shapesGraph` and component
//! parameters) is done by substituting constants for the variables in the planner
//! (`Planner::subst`), so blank node focus nodes work too. `$PATH` is replaced
//! textually with the SPARQL form of the shape's path before parsing.

use crate::path::PropertyPath;
use crate::shapes::{G, ShapeId};
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
fn prefixes(g: &G<'_>, node: &Term) -> Vec<(String, String)> {
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

/// Reject query forms that are incompatible with pre-binding (SHACL §5.2.1).
fn check_prebinding(gp: &GraphPattern, prebound: &[String]) -> Result<()> {
    use GraphPattern as GP;
    match gp {
        GP::Minus { .. } => bail!("MINUS is not allowed in SHACL-SPARQL queries"),
        GP::Service { .. } => bail!("SERVICE is not allowed in SHACL-SPARQL queries"),
        GP::Values { variables, .. } => {
            if let Some(v) = variables
                .iter()
                .find(|v| prebound.iter().any(|p| p == v.as_str()))
            {
                bail!(
                    "VALUES must not mention the pre-bound variable ?{}",
                    v.as_str()
                );
            }
            // VALUES is disallowed with pre-binding in general (W3C test unsupported-sparql-002)
            bail!("VALUES is not allowed in SHACL-SPARQL queries")
        }
        GP::Extend {
            inner, variable, ..
        }
        | GP::Assign {
            inner, variable, ..
        }
        | GP::Unfold {
            inner, variable, ..
        } => {
            if prebound.iter().any(|p| p == variable.as_str()) {
                bail!(
                    "the pre-bound variable ?{} must not be assigned with AS",
                    variable.as_str()
                );
            }
            check_prebinding(inner, prebound)
        }
        GP::Join { left, right } | GP::Lateral { left, right } | GP::Union { left, right } => {
            check_prebinding(left, prebound)?;
            check_prebinding(right, prebound)
        }
        GP::LeftJoin { left, right, .. } => {
            check_prebinding(left, prebound)?;
            check_prebinding(right, prebound)
        }
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Group { inner, .. } => check_prebinding(inner, prebound),
        _ => Ok(()),
    }
}

fn query_pattern(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

fn parse(text: &str, prefixes: &[(String, String)], prebound: &[String]) -> Result<Query> {
    let q = parse_query(text, None, prefixes)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("invalid SHACL-SPARQL query:\n{text}"))?;
    check_prebinding(query_pattern(&q), prebound)?;
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
    fn run_query(&self, q: &Query, binds: &[(&str, Term)]) -> Result<(Table, Ctx)> {
        let depth = depth::check_query(q).map_err(|e| anyhow!("{e}"))?;
        depth::with_stack(depth, || self.run_query_on(q, binds))
    }

    fn run_query_on(&self, q: &Query, binds: &[(&str, Term)]) -> Result<(Table, Ctx)> {
        let mut ctx = Ctx::new(self.data.snap.clone());
        ctx.use_cache = false;
        ctx.deadline = self.deadline;
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
        if self.shapes.bnodes_in_store {
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

    fn result_term(ctx: &Ctx, id: Id) -> Option<Term> {
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
}
