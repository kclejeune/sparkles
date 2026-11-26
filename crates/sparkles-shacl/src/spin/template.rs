//! SPIN template call expansion (`spin:Template`, `spin:ConstructTemplate`, `spin:body`,
//! `spl:Argument`, `spl:predicate`, `spl:defaultValue`, `spl:optional`, `spl:valueType`)
//! with `rdfs:subClassOf` inheritance and cycle detection.

use super::rdf::{
    MAX_DEPTH, has_rdf_query_structure, push_str_bounded, serialize_query_with_bindings,
    spin_var_name, validate_var_name, write_term_value,
};
use super::vocab::{sp, spin, spl};
use crate::shapes::{G, literal_bool};
use crate::vocab::{rdf, rdfs};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::{NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug)]
struct TemplateArg {
    predicate: NamedNode,
    var: String,
    optional: Option<bool>,
    default_value: Option<Term>,
    value_type: Option<NamedNode>,
}

struct TemplateDef {
    body: Term,
    args: Vec<TemplateArg>,
}

fn local_name(iri: &str) -> &str {
    let i = iri.rfind(['#', '/', ':']).map_or(0, |i| i + 1);
    &iri[i..]
}

fn is_direct_sp_query_class(iri: &str) -> bool {
    matches!(
        iri,
        "http://spinrdf.org/sp#Query"
            | "http://spinrdf.org/sp#Construct"
            | "http://spinrdf.org/sp#Select"
            | "http://spinrdf.org/sp#Ask"
            | "http://spinrdf.org/sp#Describe"
    )
}

fn is_spin_template_base(iri: &str) -> bool {
    matches!(
        iri,
        "http://spinrdf.org/spin#Template"
            | "http://spinrdf.org/spin#ConstructTemplate"
            | "http://spinrdf.org/spin#SelectTemplate"
            | "http://spinrdf.org/spin#AskTemplate"
            | "http://spinrdf.org/spin#Templates"
            | "http://spinrdf.org/spin#ConstructTemplates"
            | "http://spinrdf.org/spin#SelectTemplates"
            | "http://spinrdf.org/spin#AskTemplates"
    )
}

fn direct_superclasses(g: &G<'_>, cls: &NamedNode) -> Vec<NamedNode> {
    let mut supers: Vec<NamedNode> = g
        .objects(&Term::NamedNode(cls.clone()), rdfs::SUB_CLASS_OF)
        .into_iter()
        .filter_map(|t| match t {
            Term::NamedNode(n) => Some(n),
            _ => None,
        })
        .collect();
    supers.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    supers.dedup();
    supers
}

/// Check that the `rdfs:subClassOf` graph reachable from `root` is acyclic and within
/// [`MAX_DEPTH`], allowing valid diamond inheritance.
fn check_subclass_acyclic(
    g: &G<'_>,
    cls: &NamedNode,
    depth: usize,
    ancestors: &mut FxHashSet<NamedNode>,
    verified: &mut FxHashSet<NamedNode>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!(
            "SPIN template rdfs:subClassOf hierarchy exceeds maximum depth ({MAX_DEPTH}) at {cls}"
        );
    }
    if !ancestors.insert(cls.clone()) {
        bail!("cyclic rdfs:subClassOf hierarchy in SPIN template {cls}");
    }
    if verified.contains(cls) {
        ancestors.remove(cls);
        return Ok(());
    }
    for sup in direct_superclasses(g, cls) {
        check_subclass_acyclic(g, &sup, depth + 1, ancestors, verified)?;
    }
    ancestors.remove(cls);
    verified.insert(cls.clone());
    Ok(())
}

/// Collect `root` and all its `rdfs:subClassOf*` ancestors grouped by BFS distance (nearest
/// first), after verifying acyclicity and depth bounds.
fn collect_hierarchy_layers(g: &G<'_>, root: &NamedNode) -> Result<Vec<Vec<NamedNode>>> {
    let mut ancestors = FxHashSet::default();
    let mut verified = FxHashSet::default();
    check_subclass_acyclic(g, root, 0, &mut ancestors, &mut verified)?;

    let mut layers: Vec<Vec<NamedNode>> = Vec::new();
    let mut seen: FxHashSet<NamedNode> = FxHashSet::default();
    let mut queue: VecDeque<(NamedNode, usize)> = VecDeque::new();
    seen.insert(root.clone());
    queue.push_back((root.clone(), 0));
    while let Some((cls, dist)) = queue.pop_front() {
        if layers.len() == dist {
            layers.push(Vec::new());
        }
        layers[dist].push(cls.clone());
        for sup in direct_superclasses(g, &cls) {
            if seen.insert(sup.clone()) {
                queue.push_back((sup, dist + 1));
            }
        }
    }
    for layer in &mut layers {
        layer.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    }
    Ok(layers)
}

fn class_is_template(g: &G<'_>, cls: &NamedNode) -> Result<bool> {
    if is_direct_sp_query_class(cls.as_str()) {
        return Ok(false);
    }
    let layers = collect_hierarchy_layers(g, cls)?;
    for layer in &layers {
        for c in layer {
            if is_spin_template_base(c.as_str()) {
                return Ok(true);
            }
            let term = Term::NamedNode(c.clone());
            if g.has(&term, spin::BODY)
                || g.is_instance(&term, spin::TEMPLATE)
                || g.is_instance(&term, spin::CONSTRUCT_TEMPLATE)
                || g.is_instance(&term, spin::SELECT_TEMPLATE)
                || g.is_instance(&term, spin::ASK_TEMPLATE)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Return the template class IRI if `node` is an instance of a SPIN template class.
pub(crate) fn template_of_call(g: &G<'_>, node: &Term) -> Result<Option<NamedNode>> {
    let mut types: Vec<NamedNode> = g
        .objects(node, rdf::TYPE)
        .into_iter()
        .filter_map(|t| match t {
            Term::NamedNode(n) => Some(n),
            _ => None,
        })
        .filter(|n| {
            !is_direct_sp_query_class(n.as_str())
                && !matches!(
                    n.as_str(),
                    "http://www.w3.org/2000/01/rdf-schema#Resource"
                        | "http://www.w3.org/2002/07/owl#Thing"
                )
        })
        .collect();
    types.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    types.dedup();

    let mut templates = Vec::new();
    for ty in types {
        if class_is_template(g, &ty)? {
            templates.push(ty);
        }
    }
    match templates.len() {
        0 => Ok(None),
        1 => Ok(Some(templates.pop().unwrap())),
        _ => {
            // Keep only the most specific template(s) (those not a superclass of another)
            let mut most_specific = Vec::new();
            for candidate in &templates {
                let mut is_super_of_another = false;
                for other in &templates {
                    if other == candidate {
                        continue;
                    }
                    let layers = collect_hierarchy_layers(g, other)?;
                    if layers.iter().flatten().any(|c| c == candidate) {
                        is_super_of_another = true;
                        break;
                    }
                }
                if !is_super_of_another {
                    most_specific.push(candidate.clone());
                }
            }
            if most_specific.len() == 1 {
                Ok(Some(most_specific.pop().unwrap()))
            } else {
                bail!("SPIN template call {node} has multiple unrelated template types")
            }
        }
    }
}

fn single_opt(g: &G<'_>, node: &Term, pred: oxrdf::NamedNodeRef<'_>) -> Result<Option<Term>> {
    let mut vals = g.objects(node, pred);
    match vals.len() {
        0 => Ok(None),
        1 => Ok(vals.pop()),
        _ => bail!(
            "SPIN node {node} has multiple values for <{}>",
            pred.as_str()
        ),
    }
}

fn resolve_template(g: &G<'_>, template: &NamedNode) -> Result<TemplateDef> {
    let layers = collect_hierarchy_layers(g, template)?;

    // Nearest layer with a spin:body wins
    let mut body: Option<Term> = None;
    for layer in &layers {
        let mut found: Vec<Term> = Vec::new();
        for cls in layer {
            let bodies = g.objects(&Term::NamedNode(cls.clone()), spin::BODY);
            if bodies.len() > 1 {
                bail!("SPIN template {cls} has multiple spin:body values");
            }
            found.extend(bodies);
        }
        found.sort_by_cached_key(ToString::to_string);
        found.dedup();
        if found.len() > 1 {
            bail!("SPIN template {template} inherits conflicting spin:body definitions");
        }
        if let Some(b) = found.pop() {
            body = Some(b);
            break;
        }
    }
    let body = body.ok_or_else(|| anyhow!("SPIN template {template} has no spin:body"))?;

    // Collect spl:Arguments from most specific to least specific; subclass overrides superclass
    let mut args_by_pred: BTreeMap<NamedNode, TemplateArg> = BTreeMap::new();
    for layer in &layers {
        for cls in layer {
            let mut constraints = g.objects(&Term::NamedNode(cls.clone()), spin::CONSTRAINT);
            constraints.sort_by_cached_key(ToString::to_string);
            for c in constraints {
                if !g.is_instance(&c, spl::ARGUMENT) {
                    continue;
                }
                let pred = match single_opt(g, &c, spl::PREDICATE)? {
                    Some(Term::NamedNode(p)) => p,
                    Some(other) => {
                        bail!(
                            "spl:predicate of argument {c} in {cls} must be an IRI, found {other}"
                        )
                    }
                    None => {
                        bail!("spl:Argument {c} of SPIN template {cls} is missing spl:predicate")
                    }
                };
                let var = local_name(pred.as_str()).to_string();
                validate_var_name(&var).with_context(|| {
                    format!("spl:predicate {pred} of SPIN template {cls} has an invalid SPARQL variable local name")
                })?;
                let optional = match single_opt(g, &c, spl::OPTIONAL)? {
                    None => None,
                    Some(ref t) => Some(literal_bool(t).ok_or_else(|| {
                        anyhow!("spl:optional of argument {c} in {cls} must be an xsd:boolean literal, found {t}")
                    })?),
                };
                let default_value = single_opt(g, &c, spl::DEFAULT_VALUE)?;
                let value_type = match single_opt(g, &c, spl::VALUE_TYPE)? {
                    None => None,
                    Some(Term::NamedNode(vt)) => Some(vt),
                    Some(other) => {
                        bail!(
                            "spl:valueType of argument {c} in {cls} must be an IRI, found {other}"
                        )
                    }
                };
                match args_by_pred.get_mut(&pred) {
                    None => {
                        args_by_pred.insert(
                            pred.clone(),
                            TemplateArg {
                                predicate: pred,
                                var,
                                optional,
                                default_value,
                                value_type,
                            },
                        );
                    }
                    Some(existing) => {
                        if existing.optional.is_none() {
                            existing.optional = optional;
                        }
                        if existing.default_value.is_none() {
                            existing.default_value = default_value;
                        }
                        if existing.value_type.is_none() {
                            existing.value_type = value_type;
                        }
                    }
                }
            }
        }
    }

    // Check that no two distinct predicates map to the same variable name
    let mut vars_seen: FxHashMap<&str, &NamedNode> = FxHashMap::default();
    for arg in args_by_pred.values() {
        if let Some(prev) = vars_seen.insert(arg.var.as_str(), &arg.predicate) {
            bail!(
                "SPIN template {template} has ambiguous arguments {prev} and {} mapping to ?{}",
                arg.predicate,
                arg.var
            );
        }
    }

    Ok(TemplateDef {
        body,
        args: args_by_pred.into_values().collect(),
    })
}

fn check_value_type(val: &Term, value_type: &NamedNode, pred: &NamedNode) -> Result<()> {
    let vt = value_type.as_str();
    if vt == "http://www.w3.org/2000/01/rdf-schema#Resource" {
        return Ok(());
    }
    if vt == "http://www.w3.org/2000/01/rdf-schema#Literal" {
        if !matches!(val, Term::Literal(_)) {
            bail!("SPIN template argument {pred} expects a literal (rdfs:Literal), found {val}");
        }
        return Ok(());
    }
    let is_datatype = vt.starts_with("http://www.w3.org/2001/XMLSchema#")
        || matches!(
            vt,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#HTML"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral"
        );
    if is_datatype {
        let Term::Literal(lit) = val else {
            bail!(
                "SPIN template argument {pred} expects a literal of datatype {value_type}, found {val}"
            );
        };
        if lit.datatype().as_str() != vt {
            bail!(
                "SPIN template argument {pred} expects datatype {value_type}, found {}",
                lit.datatype()
            );
        }
        if vt.starts_with("http://www.w3.org/2001/XMLSchema#") && !crate::is_valid_literal(lit) {
            bail!(
                "SPIN template argument {pred} has lexically invalid literal {lit} for {value_type}"
            );
        }
        return Ok(());
    }
    // Any other class IRI (rdfs:Class, rdf:Property, owl:Class, domain class) requires a non-literal resource
    if matches!(val, Term::Literal(_)) {
        bail!(
            "SPIN template argument {pred} expects a resource of type {value_type}, found literal {val}"
        );
    }
    Ok(())
}

/// Expand a SPIN template call into a concrete SPARQL 1.1 query string.
pub(crate) fn expand_template_call(g: &G<'_>, call: &Term, template: &NamedNode) -> Result<String> {
    let outer = FxHashMap::default();
    let mut active = FxHashSet::default();
    expand_inner(g, call, template, &outer, &mut active, 0)
}

fn expand_inner(
    g: &G<'_>,
    call: &Term,
    template: &NamedNode,
    outer_bindings: &FxHashMap<String, Option<Term>>,
    active_templates: &mut FxHashSet<NamedNode>,
    depth: usize,
) -> Result<String> {
    if depth > MAX_DEPTH {
        bail!("SPIN template expansion exceeds maximum depth ({MAX_DEPTH}) at {template}");
    }
    if !active_templates.insert(template.clone()) {
        bail!("cyclic SPIN template expansion involving {template}");
    }
    let result = (|| {
        let def = resolve_template(g, template)?;
        let mut bindings: FxHashMap<String, Option<Term>> = FxHashMap::default();
        for arg in &def.args {
            let vals = g.objects(call, arg.predicate.as_ref());
            if vals.len() > 1 {
                bail!(
                    "SPIN template call {call} of {template} has multiple values for argument {}",
                    arg.predicate
                );
            }
            let raw = if let Some(v) = vals.into_iter().next() {
                // In nested template calls, allow forwarding an outer template variable
                if !outer_bindings.is_empty()
                    && let Some(var_name) = spin_var_name(g, &v)?
                    && let Some(outer_val) = outer_bindings.get(&var_name)
                {
                    outer_val.clone()
                } else {
                    Some(v)
                }
            } else if let Some(default) = &arg.default_value {
                Some(default.clone())
            } else if arg.optional == Some(true) {
                None
            } else {
                bail!(
                    "SPIN template call {call} of {template} is missing required argument {}",
                    arg.predicate
                );
            };
            if let Some(ref val) = raw {
                if matches!(val, Term::BlankNode(_)) {
                    bail!(
                        "SPIN template call {call} of {template} cannot bind blank node {val} to argument {}",
                        arg.predicate
                    );
                }
                if let Some(ref vt) = arg.value_type {
                    check_value_type(val, vt, &arg.predicate)?;
                }
            }
            bindings.insert(arg.var.clone(), raw);
        }

        // The body may itself be another template call, SPIN RDF syntax, or sp:text
        if let Some(inner_template) = template_of_call(g, &def.body)? {
            return expand_inner(
                g,
                &def.body,
                &inner_template,
                &bindings,
                active_templates,
                depth + 1,
            );
        }
        if has_rdf_query_structure(g, &def.body) {
            return serialize_query_with_bindings(g, &def.body, &bindings);
        }
        match single_opt(g, &def.body, sp::TEXT)? {
            Some(Term::Literal(lit)) => substitute_sparql_text(lit.value(), &bindings),
            Some(other) => bail!(
                "the sp:text of spin:body {} in SPIN template {template} is {other}, not a string",
                def.body
            ),
            None => bail!(
                "the spin:body {} of SPIN template {template} has neither SPIN RDF syntax nor sp:text",
                def.body
            ),
        }
    })();
    active_templates.remove(template);
    result
}

/// Skip ASCII whitespace and SPARQL `# ... \n` comments starting at `pos`.
fn skip_ws_and_comments(text: &str, mut pos: usize) -> usize {
    let b = text.as_bytes();
    while pos < b.len() {
        if b[pos].is_ascii_whitespace() {
            pos += 1;
        } else if b[pos] == b'#' {
            while pos < b.len() && b[pos] != b'\n' {
                pos += 1;
            }
        } else {
            break;
        }
    }
    pos
}

/// Read a SPARQL variable (`?name` or `$name`) starting at `pos`, returning `(name, end_pos)`.
fn scan_var(text: &str, pos: usize) -> Option<(&str, usize)> {
    let rest = text.get(pos..)?;
    if !rest.starts_with('?') && !rest.starts_with('$') {
        return None;
    }
    let after = &rest[1..];
    let mut len = 0;
    for c in after.chars() {
        if crate::sparql::varname_char(c) {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if len == 0 {
        return None;
    }
    Some((&after[..len], pos + 1 + len))
}

fn is_keyword_boundary_before(text: &str, pos: usize) -> bool {
    match text[..pos].chars().next_back() {
        None => true,
        Some(c) => !crate::sparql::varname_char(c) && c != ':' && c != '?' && c != '$',
    }
}

fn is_keyword_boundary_after(text: &str, end: usize) -> bool {
    match text[end..].chars().next() {
        None => true,
        Some(c) => !crate::sparql::varname_char(c) && c != ':',
    }
}

/// Token-aware substitution of template argument bindings into a SPARQL `sp:text` query.
///
/// - Leaves string literals (`'...'`, `"..."`, `'''...'''`, `"""..."""`), IRI references
///   (`<...>`), and `#` comments untouched.
/// - Rewrites `BOUND(?arg)` / `BOUND($arg)` to `true` when `?arg` is bound (SPARQL forbids
///   `BOUND(<const>)`).
/// - Refuses `AS ?arg` / `AS $arg` when `?arg` is bound.
/// - Substitutes bound `?arg` / `$arg` with their serialized SPARQL term; leaves optional
///   unbound arguments as `?arg` / `$arg`.
pub(crate) fn substitute_sparql_text(
    text: &str,
    bindings: &FxHashMap<String, Option<Term>>,
) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut copied = 0usize;
    let mut i = 0usize;

    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                let long = b[i..].starts_with(&[q, q, q]);
                let delim = if long { 3 } else { 1 };
                let mut j = i + delim;
                while j < b.len() {
                    if b[j] == b'\\' {
                        let next_len = text
                            .get(j + 1..)
                            .and_then(|s| s.chars().next())
                            .map_or(0, char::len_utf8);
                        j = (j + 1 + next_len).min(b.len());
                    } else if b[j..].starts_with(&[q, q, q][..delim]) {
                        j += delim;
                        break;
                    } else if !long && b[j] == b'\n' {
                        break;
                    } else {
                        let ch_len = text[j..].chars().next().map_or(1, char::len_utf8);
                        j += ch_len;
                    }
                }
                i = j.min(b.len());
            }
            b'<' => {
                let mut j = i + 1;
                while let Some(&c) = b.get(j) {
                    if c == b'>'
                        || c <= b' '
                        || matches!(c, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`')
                    {
                        break;
                    }
                    let ch_len = text[j..].chars().next().map_or(1, char::len_utf8);
                    j += ch_len;
                }
                i = if b.get(j) == Some(&b'>') {
                    j + 1
                } else {
                    i + 1
                };
            }
            b'B' | b'b'
                if b[i..].len() >= 5
                    && b[i..i + 5].eq_ignore_ascii_case(b"BOUND")
                    && is_keyword_boundary_before(text, i)
                    && is_keyword_boundary_after(text, i + 5) =>
            {
                let p = skip_ws_and_comments(text, i + 5);
                if b.get(p) == Some(&b'(') {
                    let v_start = skip_ws_and_comments(text, p + 1);
                    if let Some((var, v_end)) = scan_var(text, v_start) {
                        let close = skip_ws_and_comments(text, v_end);
                        if b.get(close) == Some(&b')')
                            && bindings.get(var).is_some_and(Option::is_some)
                        {
                            push_str_bounded(&mut out, &text[copied..i])?;
                            push_str_bounded(&mut out, "true")?;
                            i = close + 1;
                            copied = i;
                            continue;
                        }
                    }
                }
                i += 5;
            }
            b'A' | b'a'
                if b[i..].len() >= 2
                    && b[i..i + 2].eq_ignore_ascii_case(b"AS")
                    && is_keyword_boundary_before(text, i)
                    && is_keyword_boundary_after(text, i + 2) =>
            {
                let v_start = skip_ws_and_comments(text, i + 2);
                if let Some((var, _)) = scan_var(text, v_start)
                    && bindings.get(var).is_some_and(Option::is_some)
                {
                    bail!("SPIN template body assigns to bound argument ?{var} with AS");
                }
                i += 2;
            }
            b'?' | b'$' => {
                if let Some((var, end)) = scan_var(text, i) {
                    if let Some(Some(term)) = bindings.get(var) {
                        push_str_bounded(&mut out, &text[copied..i])?;
                        write_term_value(&mut out, term)?;
                        copied = end;
                    }
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => {
                // Advance one UTF-8 character safely
                let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                i += ch_len;
            }
        }
    }
    push_str_bounded(&mut out, &text[copied..])?;
    Ok(out)
}
