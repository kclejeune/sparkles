//! SPIN template call expansion (`spin:Template`, `spin:ConstructTemplate`, `spin:body`,
//! `spl:Argument`, `spl:predicate`, `spl:defaultValue`, `spl:optional`, `spl:valueType`)
//! with `rdfs:subClassOf` inheritance, work bounds, and cycle detection.

use super::ExtraTypes;
use super::rdf::{
    MAX_DEPTH, MAX_OUTPUT_BYTES, WorkBudget, has_rdf_query_structure, is_instance_bounded,
    is_limit_or_cycle_error, limit_err, parse_spin_bool, push_str_bounded,
    serialize_query_with_bindings, single_object, spin_var_name, validate_var_name,
    write_term_value,
};
use super::vocab::{sp, spin, spl};
use crate::shapes::{G, as_subject};
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

/// Per-translation cache of validated `rdfs:subClassOf` hierarchies and template-class
/// membership so multi-rule files and multi-type calls never repeat hierarchy traversals.
#[derive(Default)]
pub(crate) struct HierarchyCache {
    layers: FxHashMap<NamedNode, Vec<Vec<NamedNode>>>,
    is_template: FxHashMap<NamedNode, bool>,
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

fn is_builtin_datatype(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/2001/XMLSchema#")
        || matches!(
            iri,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#HTML"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral"
        )
}

fn direct_superclasses(
    g: &G<'_>,
    cls: &NamedNode,
    budget: &mut WorkBudget,
) -> Result<Vec<NamedNode>> {
    let mut supers: Vec<NamedNode> = Vec::new();
    for t in
        g.g.objects_for_subject_predicate(cls.as_ref(), rdfs::SUB_CLASS_OF)
    {
        budget.step()?;
        if let oxrdf::TermRef::NamedNode(n) = t {
            supers.push(n.into_owned());
        }
    }
    supers.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    supers.dedup();
    Ok(supers)
}

/// Check that the `rdfs:subClassOf` graph reachable from `root` is acyclic and within
/// [`MAX_DEPTH`] and `budget`, allowing valid diamond inheritance.
fn check_subclass_acyclic(
    g: &G<'_>,
    cls: &NamedNode,
    depth: usize,
    ancestors: &mut FxHashSet<NamedNode>,
    verified: &mut FxHashSet<NamedNode>,
    budget: &mut WorkBudget,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(limit_err(format!(
            "SPIN template rdfs:subClassOf hierarchy exceeds maximum depth ({MAX_DEPTH}) at {cls}"
        )));
    }
    budget.step()?;
    if !ancestors.insert(cls.clone()) {
        return Err(limit_err(format!(
            "cyclic rdfs:subClassOf hierarchy in SPIN template {cls}"
        )));
    }
    if verified.contains(cls) {
        ancestors.remove(cls);
        return Ok(());
    }
    for sup in direct_superclasses(g, cls, budget)? {
        check_subclass_acyclic(g, &sup, depth + 1, ancestors, verified, budget)?;
    }
    ancestors.remove(cls);
    verified.insert(cls.clone());
    Ok(())
}

/// Collect `root` and all its `rdfs:subClassOf*` ancestors grouped by BFS distance (nearest
/// first), after verifying acyclicity and depth/work bounds. Results are cached in `cache`.
fn collect_hierarchy_layers<'c>(
    g: &G<'_>,
    root: &NamedNode,
    budget: &mut WorkBudget,
    cache: &'c mut HierarchyCache,
) -> Result<&'c [Vec<NamedNode>]> {
    if !cache.layers.contains_key(root) {
        let mut ancestors = FxHashSet::default();
        let mut verified = FxHashSet::default();
        check_subclass_acyclic(g, root, 0, &mut ancestors, &mut verified, budget)?;

        let mut layers: Vec<Vec<NamedNode>> = Vec::new();
        let mut seen: FxHashSet<NamedNode> = FxHashSet::default();
        let mut queue: VecDeque<(NamedNode, usize)> = VecDeque::new();
        seen.insert(root.clone());
        queue.push_back((root.clone(), 0));
        while let Some((cls, dist)) = queue.pop_front() {
            budget.step()?;
            if layers.len() == dist {
                layers.push(Vec::new());
            }
            layers[dist].push(cls.clone());
            for sup in direct_superclasses(g, &cls, budget)? {
                if seen.insert(sup.clone()) {
                    queue.push_back((sup, dist + 1));
                }
            }
        }
        for layer in &mut layers {
            layer.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        }
        cache.layers.insert(root.clone(), layers);
    } else {
        budget.step()?;
    }
    Ok(cache.layers[root].as_slice())
}

fn class_is_template(
    g: &G<'_>,
    cls: &NamedNode,
    budget: &mut WorkBudget,
    cache: &mut HierarchyCache,
) -> Result<bool> {
    if is_direct_sp_query_class(cls.as_str()) {
        return Ok(false);
    }
    if let Some(&cached) = cache.is_template.get(cls) {
        budget.step()?;
        return Ok(cached);
    }
    let layers = collect_hierarchy_layers(g, cls, budget, cache)?.to_vec();
    let mut found = false;
    'outer: for layer in &layers {
        for c in layer {
            budget.step()?;
            if is_spin_template_base(c.as_str()) {
                found = true;
                break 'outer;
            }
            let term = Term::NamedNode(c.clone());
            if g.has(&term, spin::BODY)
                || is_instance_bounded(g, &term, spin::TEMPLATE, budget)?
                || is_instance_bounded(g, &term, spin::CONSTRUCT_TEMPLATE, budget)?
                || is_instance_bounded(g, &term, spin::SELECT_TEMPLATE, budget)?
                || is_instance_bounded(g, &term, spin::ASK_TEMPLATE, budget)?
            {
                found = true;
                break 'outer;
            }
        }
    }
    cache.is_template.insert(cls.clone(), found);
    Ok(found)
}

/// Return the template class IRI if `node` is an instance of a SPIN template class.
pub(crate) fn template_of_call(
    g: &G<'_>,
    node: &Term,
    budget: &mut WorkBudget,
    cache: &mut HierarchyCache,
) -> Result<Option<NamedNode>> {
    let Some(sub) = as_subject(node) else {
        return Ok(None);
    };
    let mut types: Vec<NamedNode> = Vec::new();
    for t in g.g.objects_for_subject_predicate(sub, rdf::TYPE) {
        budget.step()?;
        if let oxrdf::TermRef::NamedNode(n) = t
            && !is_direct_sp_query_class(n.as_str())
            && !matches!(
                n.as_str(),
                "http://www.w3.org/2000/01/rdf-schema#Resource"
                    | "http://www.w3.org/2002/07/owl#Thing"
            )
        {
            types.push(n.into_owned());
        }
    }
    types.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    types.dedup();

    let mut templates = Vec::new();
    for ty in types {
        if class_is_template(g, &ty, budget, cache)? {
            templates.push(ty);
        }
    }
    match templates.len() {
        0 => Ok(None),
        1 => Ok(Some(templates.pop().unwrap())),
        _ => {
            // Keep only the most specific template(s) (not a strict superclass of another).
            // Each candidate's hierarchy is already cached in `cache.layers`, so one pass
            // over each template's strict ancestors runs in O(sum of hierarchy sizes).
            let template_set: FxHashSet<NamedNode> = templates.iter().cloned().collect();
            let mut is_super: FxHashSet<NamedNode> = FxHashSet::default();
            for other in &templates {
                let layers = collect_hierarchy_layers(g, other, budget, cache)?;
                for ancestor in layers.iter().skip(1).flatten() {
                    budget.step()?;
                    if template_set.contains(ancestor) {
                        is_super.insert(ancestor.clone());
                    }
                }
            }
            let mut most_specific: Vec<NamedNode> = templates
                .into_iter()
                .filter(|c| !is_super.contains(c))
                .collect();
            if most_specific.len() == 1 {
                Ok(Some(most_specific.pop().unwrap()))
            } else {
                bail!("SPIN template call {node} has multiple unrelated template types")
            }
        }
    }
}

fn resolve_template(
    g: &G<'_>,
    template: &NamedNode,
    budget: &mut WorkBudget,
    cache: &mut HierarchyCache,
) -> Result<TemplateDef> {
    let layers = collect_hierarchy_layers(g, template, budget, cache)?.to_vec();

    // Nearest layer with a spin:body wins
    let mut body: Option<Term> = None;
    for layer in &layers {
        let mut found: Vec<Term> = Vec::new();
        for cls in layer {
            if let Some(b) = single_object(g, &Term::NamedNode(cls.clone()), spin::BODY, budget)? {
                found.push(b);
            }
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

    // Collect spl:Arguments layer by layer from most specific to least specific.
    // Conflicting metadata for the same spl:predicate within one class or one BFS layer is
    // refused unless already overridden by a more specific layer.
    let mut args_by_pred: BTreeMap<NamedNode, TemplateArg> = BTreeMap::new();
    for layer in &layers {
        let mut layer_args: BTreeMap<NamedNode, TemplateArg> = BTreeMap::new();
        for cls in layer {
            let mut constraints: Vec<Term> = Vec::new();
            for c in
                g.g.objects_for_subject_predicate(cls.as_ref(), spin::CONSTRAINT)
            {
                budget.step()?;
                constraints.push(c.into_owned());
            }
            constraints.sort_by_cached_key(ToString::to_string);
            for c in constraints {
                if !is_instance_bounded(g, &c, spl::ARGUMENT, budget)? {
                    continue;
                }
                let pred = match single_object(g, &c, spl::PREDICATE, budget)? {
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
                    format!(
                        "spl:predicate {pred} of SPIN template {cls} has an invalid SPARQL variable local name"
                    )
                })?;
                if var == "this" {
                    bail!(
                        "spl:predicate {pred} of SPIN template {cls} cannot use the reserved variable name ?this"
                    );
                }
                let optional = match single_object(g, &c, spl::OPTIONAL, budget)? {
                    None => None,
                    Some(ref t) => Some(parse_spin_bool(
                        t,
                        &format!("spl:optional of argument {c} in {cls}"),
                    )?),
                };
                let default_value = single_object(g, &c, spl::DEFAULT_VALUE, budget)?;
                let value_type = match single_object(g, &c, spl::VALUE_TYPE, budget)? {
                    None => None,
                    Some(Term::NamedNode(vt)) => Some(vt),
                    Some(other) => {
                        bail!(
                            "spl:valueType of argument {c} in {cls} must be an IRI, found {other}"
                        )
                    }
                };
                let prev_resolved = args_by_pred.get(&pred);
                match layer_args.get_mut(&pred) {
                    None => {
                        layer_args.insert(
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
                        if let Some(opt) = optional
                            && prev_resolved.is_none_or(|p| p.optional.is_none())
                        {
                            if let Some(prev) = existing.optional
                                && prev != opt
                            {
                                bail!(
                                    "SPIN template {template} has conflicting spl:optional values for argument {pred}"
                                );
                            }
                            existing.optional = Some(opt);
                        }
                        if let Some(def) = default_value
                            && prev_resolved.is_none_or(|p| p.default_value.is_none())
                        {
                            if let Some(ref prev) = existing.default_value
                                && prev != &def
                            {
                                bail!(
                                    "SPIN template {template} has conflicting spl:defaultValue values for argument {pred}"
                                );
                            }
                            existing.default_value = Some(def);
                        }
                        if let Some(vt) = value_type
                            && prev_resolved.is_none_or(|p| p.value_type.is_none())
                        {
                            if let Some(ref prev) = existing.value_type
                                && prev != &vt
                            {
                                bail!(
                                    "SPIN template {template} has conflicting spl:valueType values for argument {pred}"
                                );
                            }
                            existing.value_type = Some(vt);
                        }
                    }
                }
            }
        }
        for (pred, arg) in layer_args {
            match args_by_pred.get_mut(&pred) {
                None => {
                    args_by_pred.insert(pred, arg);
                }
                Some(existing) => {
                    if existing.optional.is_none() {
                        existing.optional = arg.optional;
                    }
                    if existing.default_value.is_none() {
                        existing.default_value = arg.default_value;
                    }
                    if existing.value_type.is_none() {
                        existing.value_type = arg.value_type;
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

fn builtin_metaclass_super(iri: &str) -> Option<&'static str> {
    match iri {
        "http://www.w3.org/2002/07/owl#Class" | "http://www.w3.org/2000/01/rdf-schema#Datatype" => {
            Some("http://www.w3.org/2000/01/rdf-schema#Class")
        }
        "http://www.w3.org/2002/07/owl#Restriction" => Some("http://www.w3.org/2002/07/owl#Class"),
        "http://www.w3.org/2002/07/owl#ObjectProperty"
        | "http://www.w3.org/2002/07/owl#DatatypeProperty"
        | "http://www.w3.org/2002/07/owl#AnnotationProperty"
        | "http://www.w3.org/2002/07/owl#FunctionalProperty"
        | "http://www.w3.org/2002/07/owl#InverseFunctionalProperty"
        | "http://www.w3.org/2002/07/owl#TransitiveProperty"
        | "http://www.w3.org/2002/07/owl#SymmetricProperty"
        | "http://www.w3.org/2002/07/owl#AsymmetricProperty"
        | "http://www.w3.org/2002/07/owl#ReflexiveProperty"
        | "http://www.w3.org/2002/07/owl#IrreflexiveProperty" => {
            Some("http://www.w3.org/1999/02/22-rdf-syntax-ns#Property")
        }
        _ => None,
    }
}

/// Check `val rdf:type/rdfs:subClassOf* target_class` across `g` and optional `extra`
/// data-graph index lookups, including built-in RDFS/OWL meta-class inclusions, under
/// `budget`.
fn is_class_member(
    g: &G<'_>,
    extra: Option<&ExtraTypes<'_>>,
    val: &Term,
    target_class: &NamedNode,
    budget: &mut WorkBudget,
) -> Result<bool> {
    if target_class.as_str() == "http://www.w3.org/2000/01/rdf-schema#Resource" {
        return Ok(true);
    }
    let target = Term::NamedNode(target_class.clone());
    let mut stack: Vec<Term> = Vec::new();
    if let Some(sub) = as_subject(val) {
        for obj in g.g.objects_for_subject_predicate(sub, rdf::TYPE) {
            budget.step()?;
            if obj == target.as_ref() {
                return Ok(true);
            }
            stack.push(obj.into_owned());
        }
    }
    if let Some(ex) = extra {
        for obj in ex.rdf_types(val, budget)? {
            if obj == target {
                return Ok(true);
            }
            stack.push(obj);
        }
    }
    if let Term::NamedNode(n) = val
        && is_builtin_datatype(n.as_str())
    {
        let dt = Term::NamedNode(rdfs::DATATYPE.into_owned());
        if dt == target {
            return Ok(true);
        }
        stack.push(dt);
    }

    let mut seen: FxHashSet<Term> = FxHashSet::default();
    while let Some(cls) = stack.pop() {
        if !seen.insert(cls.clone()) {
            continue;
        }
        if let Term::NamedNode(ref n) = cls
            && let Some(sup_iri) = builtin_metaclass_super(n.as_str())
        {
            let sup = Term::NamedNode(NamedNode::new_unchecked(sup_iri));
            if sup == target {
                return Ok(true);
            }
            stack.push(sup);
        }
        if let Some(c_sub) = as_subject(&cls) {
            for sup in g.g.objects_for_subject_predicate(c_sub, rdfs::SUB_CLASS_OF) {
                budget.step()?;
                if sup == target.as_ref() {
                    return Ok(true);
                }
                stack.push(sup.into_owned());
            }
        }
        if let Some(ex) = extra {
            for sup in ex.superclasses(&cls, budget)? {
                if sup == target {
                    return Ok(true);
                }
                stack.push(sup);
            }
        }
    }
    Ok(false)
}

fn check_value_type(
    g: &G<'_>,
    extra: Option<&ExtraTypes<'_>>,
    val: &Term,
    value_type: &NamedNode,
    pred: &NamedNode,
    budget: &mut WorkBudget,
) -> Result<()> {
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
    let is_datatype = is_builtin_datatype(vt)
        || is_class_member(
            g,
            extra,
            &Term::NamedNode(value_type.clone()),
            &rdfs::DATATYPE.into_owned(),
            budget,
        )?;
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
    if matches!(val, Term::Literal(_)) {
        bail!(
            "SPIN template argument {pred} expects a resource of type {value_type}, found literal {val}"
        );
    }
    if !is_class_member(g, extra, val, value_type, budget)? {
        bail!("SPIN template argument {pred} expects an instance of {value_type}, found {val}");
    }
    Ok(())
}

/// Expand a SPIN template call into a concrete SPARQL 1.1 query string.
pub(crate) fn expand_template_call(
    g: &G<'_>,
    extra: Option<&ExtraTypes<'_>>,
    call: &Term,
    template: &NamedNode,
    budget: &mut WorkBudget,
    cache: &mut HierarchyCache,
) -> Result<String> {
    let outer = FxHashMap::default();
    let mut active = FxHashSet::default();
    expand_inner(
        g,
        extra,
        call,
        template,
        &outer,
        &mut active,
        0,
        budget,
        cache,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand_inner(
    g: &G<'_>,
    extra: Option<&ExtraTypes<'_>>,
    call: &Term,
    template: &NamedNode,
    outer_bindings: &FxHashMap<String, Option<Term>>,
    active_templates: &mut FxHashSet<NamedNode>,
    depth: usize,
    budget: &mut WorkBudget,
    cache: &mut HierarchyCache,
) -> Result<String> {
    if depth > MAX_DEPTH {
        return Err(limit_err(format!(
            "SPIN template expansion exceeds maximum depth ({MAX_DEPTH}) at {template}"
        )));
    }
    budget.step()?;
    if !active_templates.insert(template.clone()) {
        return Err(limit_err(format!(
            "cyclic SPIN template expansion involving {template}"
        )));
    }
    let result = (|| {
        let def = resolve_template(g, template, budget, cache)?;
        let mut bindings: FxHashMap<String, Option<Term>> = FxHashMap::default();
        for arg in &def.args {
            let supplied = match single_object(g, call, arg.predicate.as_ref(), budget)? {
                None => None,
                Some(v) => {
                    if let Some(var_name) = spin_var_name(g, &v, budget)? {
                        if let Some(outer_val) = outer_bindings.get(&var_name) {
                            // Omitted outer optional argument (`None`) falls through to the
                            // inner argument's default / optional / required checks.
                            outer_val.clone()
                        } else {
                            bail!(
                                "SPIN template call {call} of {template} passes unbound SPIN variable ?{var_name} ({v}) to argument {}",
                                arg.predicate
                            );
                        }
                    } else if is_instance_bounded(g, &v, sp::VARIABLE_CLS, budget)? {
                        bail!(
                            "SPIN template call {call} of {template} passes unbound SPIN variable {v} to argument {}",
                            arg.predicate
                        );
                    } else {
                        Some(v)
                    }
                }
            };
            let raw = match supplied {
                Some(v) => Some(v),
                None => {
                    if let Some(default) = &arg.default_value {
                        Some(default.clone())
                    } else if arg.optional == Some(true) {
                        None
                    } else {
                        bail!(
                            "SPIN template call {call} of {template} is missing required argument {}",
                            arg.predicate
                        );
                    }
                }
            };
            if let Some(ref val) = raw {
                if matches!(val, Term::BlankNode(_)) {
                    bail!(
                        "SPIN template call {call} of {template} cannot bind blank node {val} to argument {} \
                         (SPIN templates expand to SPARQL query text and require IRI or literal arguments)",
                        arg.predicate
                    );
                }
                if let Some(var_name) = spin_var_name(g, val, budget)? {
                    bail!(
                        "SPIN template call {call} of {template} cannot bind SPIN variable ?{var_name} ({val}) as default value for argument {}",
                        arg.predicate
                    );
                }
                if is_instance_bounded(g, val, sp::VARIABLE_CLS, budget)? {
                    bail!(
                        "SPIN template call {call} of {template} cannot bind SPIN variable {val} as default value for argument {}",
                        arg.predicate
                    );
                }
                if let Some(ref vt) = arg.value_type {
                    check_value_type(g, extra, val, vt, &arg.predicate, budget)?;
                }
            }
            bindings.insert(arg.var.clone(), raw);
        }

        // The body may itself be another template call, SPIN RDF syntax, or sp:text
        if let Some(inner_template) = template_of_call(g, &def.body, budget, cache)? {
            return expand_inner(
                g,
                extra,
                &def.body,
                &inner_template,
                &bindings,
                active_templates,
                depth + 1,
                budget,
                cache,
            );
        }
        if has_rdf_query_structure(g, &def.body) {
            match serialize_query_with_bindings(g, &def.body, &bindings, budget) {
                Ok(q) => return Ok(q),
                Err(rdf_err) => {
                    if !is_limit_or_cycle_error(&rdf_err)
                        && let Some(Term::Literal(lit)) =
                            single_object(g, &def.body, sp::TEXT, budget)?
                    {
                        return substitute_sparql_text(lit.value(), &bindings);
                    }
                    return Err(rdf_err);
                }
            }
        }
        match single_object(g, &def.body, sp::TEXT, budget)? {
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

// --------------------------------------------------------------- SPARQL lexer & rewriter ----

fn pn_chars_base(c: char) -> bool {
    matches!(
        c,
        'A'..='Z'
            | 'a'..='z'
            | '\u{C0}'..='\u{D6}'
            | '\u{D8}'..='\u{F6}'
            | '\u{F8}'..='\u{2FF}'
            | '\u{370}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
            | '\u{10000}'..='\u{EFFFF}'
    )
}

fn pn_chars_u(c: char) -> bool {
    pn_chars_base(c) || c == '_'
}

fn pn_chars(c: char) -> bool {
    pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || matches!(c, '\u{B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}')
}

fn is_pn_local_esc(c: char) -> bool {
    matches!(
        c,
        '_' | '~'
            | '.'
            | '-'
            | '!'
            | '$'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | ';'
            | '='
            | '/'
            | '?'
            | '#'
            | '@'
            | '%'
    )
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

/// Scan an optional SPARQL 1.1 `PN_LOCAL` starting right after `:` at byte index `start`.
/// Returns the end byte offset (equal to `start` if no `PN_LOCAL` follows).
fn scan_pn_local(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    let mut j = start;
    let mut first = true;
    let mut last_valid_end = start;
    while j < b.len() {
        if b[j] == b'%' {
            if j + 2 < b.len() && b[j + 1].is_ascii_hexdigit() && b[j + 2].is_ascii_hexdigit() {
                j += 3;
                last_valid_end = j;
                first = false;
                continue;
            }
            break;
        }
        if b[j] == b'\\' {
            if let Some(esc) = text[j + 1..].chars().next()
                && is_pn_local_esc(esc)
            {
                j += 1 + esc.len_utf8();
                last_valid_end = j;
                first = false;
                continue;
            }
            break;
        }
        let Some(c) = text[j..].chars().next() else {
            break;
        };
        let ok = if first {
            pn_chars_u(c) || c == ':' || c.is_ascii_digit()
        } else {
            pn_chars(c) || c == '.' || c == ':'
        };
        if !ok {
            break;
        }
        j += c.len_utf8();
        if c != '.' {
            last_valid_end = j;
        }
        first = false;
    }
    last_valid_end
}

/// Try to scan a SPARQL 1.1 `PNAME_NS` / `PNAME_LN` starting at `start` (where `text[start..]`
/// begins with `PN_CHARS_BASE`). Returns `Some(end)` if a `:` follows a valid `PN_PREFIX`.
fn try_scan_prefixed_name(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut j = start;
    let first = text[j..].chars().next()?;
    if !pn_chars_base(first) {
        return None;
    }
    j += first.len_utf8();
    let mut last_non_dot = j;
    while j < b.len() {
        let c = text[j..].chars().next()?;
        if pn_chars(c) {
            j += c.len_utf8();
            last_non_dot = j;
        } else if c == '.' {
            j += 1;
        } else {
            break;
        }
    }
    if last_non_dot == j && b.get(j) == Some(&b':') {
        Some(scan_pn_local(text, j + 1))
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokKind {
    Word,
    Var,
    PrefixedName,
    BlankNode,
    IriRef,
    StringLit,
    LangTag,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: TokKind,
    start: usize,
    end: usize,
}

impl Tok {
    fn as_str<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.end]
    }

    fn is_word(&self, text: &str, kw: &str) -> bool {
        self.kind == TokKind::Word && self.as_str(text).eq_ignore_ascii_case(kw)
    }

    fn var_name<'a>(&self, text: &'a str) -> Option<&'a str> {
        if self.kind == TokKind::Var {
            Some(&text[self.start + 1..self.end])
        } else {
            None
        }
    }
}

/// Tokenize a SPARQL 1.1 query string into non-whitespace, non-comment tokens while
/// preserving exact byte offsets.
fn tokenize_sparql(text: &str) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let b = text.as_bytes();
    let mut i = 0usize;

    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i] == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        match b[i] {
            q @ (b'"' | b'\'') => {
                let start = i;
                let long = b[i..].starts_with(&[q, q, q]);
                if long {
                    let mut j = i + 3;
                    while j < b.len() {
                        if b[j] == b'\\' {
                            let next_len = text
                                .get(j + 1..)
                                .and_then(|s| s.chars().next())
                                .map_or(0, char::len_utf8);
                            j = (j + 1 + next_len).min(b.len());
                        } else if b[j] == q {
                            let mut run = 0usize;
                            while j + run < b.len() && b[j + run] == q {
                                run += 1;
                            }
                            if run >= 3 {
                                // SPARQL 1.1 STRING_LITERAL_LONG1/2 permits up to 2 content
                                // quotes immediately before the 3 closing quotes (3..=5 total).
                                j += run.min(5);
                                break;
                            }
                            j += run;
                        } else {
                            let ch_len = text[j..].chars().next().map_or(1, char::len_utf8);
                            j += ch_len;
                        }
                    }
                    i = j.min(b.len());
                } else {
                    let mut j = i + 1;
                    while j < b.len() {
                        if b[j] == b'\\' {
                            let next_len = text
                                .get(j + 1..)
                                .and_then(|s| s.chars().next())
                                .map_or(0, char::len_utf8);
                            j = (j + 1 + next_len).min(b.len());
                        } else if b[j] == q {
                            j += 1;
                            break;
                        } else if b[j] == b'\n' || b[j] == b'\r' {
                            break;
                        } else {
                            let ch_len = text[j..].chars().next().map_or(1, char::len_utf8);
                            j += ch_len;
                        }
                    }
                    i = j.min(b.len());
                }
                tokens.push(Tok {
                    kind: TokKind::StringLit,
                    start,
                    end: i,
                });
            }
            b'<' => {
                let start = i;
                let mut j = i + 1;
                while let Some(&c) = b.get(j) {
                    if c == b'>'
                        || c <= b' '
                        || matches!(c, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | b'\\')
                    {
                        break;
                    }
                    let ch_len = text[j..].chars().next().map_or(1, char::len_utf8);
                    j += ch_len;
                }
                if b.get(j) == Some(&b'>') {
                    i = j + 1;
                    tokens.push(Tok {
                        kind: TokKind::IriRef,
                        start,
                        end: i,
                    });
                } else {
                    i += 1;
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        start,
                        end: i,
                    });
                }
            }
            b'?' | b'$' => {
                let start = i;
                if let Some((_, end)) = scan_var(text, i) {
                    i = end;
                    tokens.push(Tok {
                        kind: TokKind::Var,
                        start,
                        end: i,
                    });
                } else {
                    i += 1;
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        start,
                        end: i,
                    });
                }
            }
            b'@' => {
                let start = i;
                if b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
                    let mut j = i + 1;
                    while j < b.len() && b[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    while j < b.len() && b[j] == b'-' {
                        let dash = j;
                        j += 1;
                        let part_start = j;
                        while j < b.len() && b[j].is_ascii_alphanumeric() {
                            j += 1;
                        }
                        if j == part_start {
                            j = dash;
                            break;
                        }
                    }
                    i = j;
                    tokens.push(Tok {
                        kind: TokKind::LangTag,
                        start,
                        end: i,
                    });
                } else {
                    i += 1;
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        start,
                        end: i,
                    });
                }
            }
            b'_' if b[i..].starts_with(b"_:") => {
                let start = i;
                let mut j = i + 2;
                let mut last_valid = j;
                if let Some(first) = text[j..].chars().next()
                    && (pn_chars_u(first) || first.is_ascii_digit())
                {
                    j += first.len_utf8();
                    last_valid = j;
                    while j < b.len() {
                        let Some(c) = text[j..].chars().next() else {
                            break;
                        };
                        if pn_chars(c) {
                            j += c.len_utf8();
                            last_valid = j;
                        } else if c == '.' {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                }
                i = last_valid;
                tokens.push(Tok {
                    kind: TokKind::BlankNode,
                    start,
                    end: i,
                });
            }
            b':' => {
                let start = i;
                i = scan_pn_local(text, i + 1);
                tokens.push(Tok {
                    kind: TokKind::PrefixedName,
                    start,
                    end: i,
                });
            }
            b'(' => {
                tokens.push(Tok {
                    kind: TokKind::LParen,
                    start: i,
                    end: i + 1,
                });
                i += 1;
            }
            b')' => {
                tokens.push(Tok {
                    kind: TokKind::RParen,
                    start: i,
                    end: i + 1,
                });
                i += 1;
            }
            b'{' => {
                tokens.push(Tok {
                    kind: TokKind::LBrace,
                    start: i,
                    end: i + 1,
                });
                i += 1;
            }
            b'}' => {
                tokens.push(Tok {
                    kind: TokKind::RBrace,
                    start: i,
                    end: i + 1,
                });
                i += 1;
            }
            _ => {
                let start = i;
                let c = text[i..].chars().next().unwrap();
                if pn_chars_base(c) {
                    if let Some(end) = try_scan_prefixed_name(text, i) {
                        i = end;
                        tokens.push(Tok {
                            kind: TokKind::PrefixedName,
                            start,
                            end: i,
                        });
                        continue;
                    }
                    let mut j = i + c.len_utf8();
                    while j < b.len() {
                        let Some(nc) = text[j..].chars().next() else {
                            break;
                        };
                        if crate::sparql::varname_char(nc) {
                            j += nc.len_utf8();
                        } else {
                            break;
                        }
                    }
                    i = j;
                    tokens.push(Tok {
                        kind: TokKind::Word,
                        start,
                        end: i,
                    });
                } else {
                    i += c.len_utf8();
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        start,
                        end: i,
                    });
                }
            }
        }
    }
    tokens
}

/// Collect every SPARQL variable name (`?name` / `$name`) referenced outside comments,
/// string literals, IRIs, and prefixed names in `text`.
pub(crate) fn collect_sparql_text_vars(text: &str, out: &mut FxHashSet<String>) {
    for tok in tokenize_sparql(text) {
        if let Some(name) = tok.var_name(text) {
            out.insert(name.to_string());
        }
    }
}

fn skip_balanced_parens(tokens: &[Tok], start: usize) -> usize {
    let mut depth = 0usize;
    let mut k = start;
    while k < tokens.len() {
        match tokens[k].kind {
            TokKind::LParen => depth += 1,
            TokKind::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return k + 1;
                }
            }
            _ => {}
        }
        k += 1;
    }
    k
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VarPosRewrite {
    Plain,
    /// Bare bound variable in `SELECT ?arg` -> `(<term> AS ?arg)`.
    SelectBind,
    /// Bare bound variable in `GROUP BY ?arg` or `ORDER BY ?arg` -> `(<term>)`.
    ParenTerm,
}

/// Token-aware substitution of template argument bindings into a SPARQL `sp:text` query.
///
/// - Tokenizes string literals (`'...'`, `"..."`, `'''...'''`, `"""..."""` including 4–5
///   closing quotes), IRI references (`<...>`), prefixed names (`PNAME_NS`/`PNAME_LN` with
///   `-`/`.`/`PLX`), blank node labels, language tags, and `#` comments atomically.
/// - Rewrites `BOUND(?arg)` / `BOUND($arg)` to `true` when `?arg` is bound.
/// - Refuses `AS ?arg` and `VALUES ... ?arg` headers when `?arg` is bound.
/// - Preserves valid SPARQL syntax in `SELECT ?arg` (`(<val> AS ?arg)`), `GROUP BY ?arg`
///   (`(<val>)`), and `ORDER BY ?arg` (`(<val>)`).
/// - Avoids merging `<` with a substituted `<iri>` into RDF-star `<<` (or `>` with `>`).
/// - Never substitutes the reserved focus variable `?this`.
pub(crate) fn substitute_sparql_text(
    text: &str,
    bindings: &FxHashMap<String, Option<Term>>,
) -> Result<String> {
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "SPIN sp:text exceeds maximum size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    let tokens = tokenize_sparql(text);
    let mut var_rewrites = vec![VarPosRewrite::Plain; tokens.len()];

    // Pre-mark bare variables in SELECT, GROUP BY, and ORDER BY clauses, and validate
    // VALUES variable headers.
    let mut idx = 0usize;
    while idx < tokens.len() {
        let tok = tokens[idx];
        if tok.is_word(text, "SELECT") {
            let mut k = idx + 1;
            if k < tokens.len()
                && (tokens[k].is_word(text, "DISTINCT") || tokens[k].is_word(text, "REDUCED"))
            {
                k += 1;
            }
            while k < tokens.len() {
                match tokens[k].kind {
                    TokKind::LParen => {
                        k = skip_balanced_parens(&tokens, k);
                    }
                    TokKind::Var => {
                        var_rewrites[k] = VarPosRewrite::SelectBind;
                        k += 1;
                    }
                    _ => break,
                }
            }
        } else if tok.is_word(text, "GROUP")
            && tokens.get(idx + 1).is_some_and(|t| t.is_word(text, "BY"))
        {
            let mut k = idx + 2;
            while k < tokens.len() {
                match tokens[k].kind {
                    TokKind::LParen => {
                        k = skip_balanced_parens(&tokens, k);
                    }
                    TokKind::Var => {
                        var_rewrites[k] = VarPosRewrite::ParenTerm;
                        k += 1;
                    }
                    TokKind::RBrace | TokKind::LBrace => break,
                    TokKind::Word
                        if tokens[k].is_word(text, "HAVING")
                            || tokens[k].is_word(text, "ORDER")
                            || tokens[k].is_word(text, "LIMIT")
                            || tokens[k].is_word(text, "OFFSET")
                            || tokens[k].is_word(text, "VALUES") =>
                    {
                        break;
                    }
                    _ => k += 1,
                }
            }
        } else if tok.is_word(text, "ORDER")
            && tokens.get(idx + 1).is_some_and(|t| t.is_word(text, "BY"))
        {
            let mut k = idx + 2;
            while k < tokens.len() {
                match tokens[k].kind {
                    TokKind::LParen => {
                        k = skip_balanced_parens(&tokens, k);
                    }
                    TokKind::Var => {
                        var_rewrites[k] = VarPosRewrite::ParenTerm;
                        k += 1;
                    }
                    TokKind::RBrace | TokKind::LBrace => break,
                    TokKind::Word
                        if tokens[k].is_word(text, "LIMIT")
                            || tokens[k].is_word(text, "OFFSET")
                            || tokens[k].is_word(text, "VALUES") =>
                    {
                        break;
                    }
                    _ => k += 1,
                }
            }
        } else if tok.is_word(text, "VALUES")
            && let Some(next) = tokens.get(idx + 1)
        {
            if let Some(var) = next.var_name(text) {
                if var != "this" && bindings.get(var).is_some_and(Option::is_some) {
                    bail!("SPIN template body uses bound argument ?{var} in VALUES");
                }
            } else if next.kind == TokKind::LParen {
                let mut k = idx + 2;
                while k < tokens.len() && tokens[k].kind != TokKind::RParen {
                    if let Some(var) = tokens[k].var_name(text)
                        && var != "this"
                        && bindings.get(var).is_some_and(Option::is_some)
                    {
                        bail!("SPIN template body uses bound argument ?{var} in VALUES");
                    }
                    k += 1;
                }
            }
        }
        idx += 1;
    }

    let mut out = String::with_capacity(text.len().min(65_536));
    let mut copied = 0usize;
    let mut i = 0usize;

    while i < tokens.len() {
        let tok = tokens[i];
        if tok.is_word(text, "BOUND")
            && tokens.get(i + 1).is_some_and(|t| t.kind == TokKind::LParen)
            && let Some(var) = tokens.get(i + 2).and_then(|t| t.var_name(text))
            && tokens.get(i + 3).is_some_and(|t| t.kind == TokKind::RParen)
            && var != "this"
            && bindings.get(var).is_some_and(Option::is_some)
        {
            push_str_bounded(&mut out, &text[copied..tok.start])?;
            push_str_bounded(&mut out, "true")?;
            copied = tokens[i + 3].end;
            i += 4;
            continue;
        }
        if tok.is_word(text, "AS")
            && let Some(var) = tokens.get(i + 1).and_then(|t| t.var_name(text))
            && var != "this"
            && bindings.get(var).is_some_and(Option::is_some)
        {
            bail!("SPIN template body assigns to bound argument ?{var} with AS");
        }
        if let Some(var) = tok.var_name(text)
            && var != "this"
            && let Some(Some(term)) = bindings.get(var)
        {
            push_str_bounded(&mut out, &text[copied..tok.start])?;
            match var_rewrites[i] {
                VarPosRewrite::SelectBind => {
                    push_str_bounded(&mut out, "(")?;
                    write_term_value(&mut out, term, 0)?;
                    push_str_bounded(&mut out, " AS ?")?;
                    push_str_bounded(&mut out, var)?;
                    push_str_bounded(&mut out, ")")?;
                }
                VarPosRewrite::ParenTerm => {
                    push_str_bounded(&mut out, "(")?;
                    write_term_value(&mut out, term, 0)?;
                    push_str_bounded(&mut out, ")")?;
                }
                VarPosRewrite::Plain => {
                    let starts_with_lt = matches!(term, Term::NamedNode(_) | Term::Triple(_));
                    if starts_with_lt && out.ends_with('<') {
                        push_str_bounded(&mut out, " ")?;
                    }
                    write_term_value(&mut out, term, 0)?;
                    let next_byte = text.as_bytes().get(tok.end).copied();
                    let needs_trailing_space = (out.ends_with('>') && next_byte == Some(b'>'))
                        || (matches!(term, Term::Literal(l) if l.language().is_some())
                            && text[tok.end..]
                                .chars()
                                .next()
                                .is_some_and(|c| crate::sparql::varname_char(c) || c == '-'));
                    if needs_trailing_space {
                        push_str_bounded(&mut out, " ")?;
                    }
                }
            }
            copied = tok.end;
        }
        i += 1;
    }
    push_str_bounded(&mut out, &text[copied..])?;
    Ok(out)
}
