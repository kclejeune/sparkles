//! SPIN template call expansion (`spin:Template`, `spin:ConstructTemplate`, `spin:body`,
//! `spl:Argument`, `spl:predicate`, `spl:defaultValue`, `spl:optional`, `spl:valueType`)
//! with `rdfs:subClassOf` inheritance, work bounds, and cycle detection.

use super::rdf::{
    MAX_DEPTH, MAX_OUTPUT_BYTES, WorkBudget, check_and_own_iri, check_and_own_term,
    has_rdf_query_structure, is_instance_bounded, is_limit_or_cycle_error, limit_err,
    map_core_parse_error, parse_spin_bool_ref, push_str_bounded, serialize_query_with_bindings,
    single_object, single_object_ref, spin_var_name, term_ref_size_and_depth, validate_var_name,
    write_term_value,
};
use super::vocab::{sp, spin, spl};
use super::{CandidateCheck, ExtraTypes};
use crate::shapes::{G, as_subject};
use crate::vocab::{rdf, rdfs};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::{NamedNode, Term, TermRef};
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
        if let TermRef::NamedNode(n) = t {
            supers.push(check_and_own_iri(n, budget)?);
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
        if let TermRef::NamedNode(n) = t
            && !is_direct_sp_query_class(n.as_str())
            && !matches!(
                n.as_str(),
                "http://www.w3.org/2000/01/rdf-schema#Resource"
                    | "http://www.w3.org/2002/07/owl#Thing"
            )
        {
            types.push(check_and_own_iri(n, budget)?);
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
                constraints.push(check_and_own_term(c, budget)?);
            }
            constraints.sort_by_cached_key(ToString::to_string);
            for c in constraints {
                if !is_instance_bounded(g, &c, spl::ARGUMENT, budget)? {
                    continue;
                }
                let pred = match single_object_ref(g, &c, spl::PREDICATE, budget)? {
                    Some(TermRef::NamedNode(p)) => check_and_own_iri(p, budget)?,
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
                let optional = match single_object_ref(g, &c, spl::OPTIONAL, budget)? {
                    None => None,
                    Some(t) => Some(parse_spin_bool_ref(
                        t,
                        &format!("spl:optional of argument {c} in {cls}"),
                    )?),
                };
                let default_value = single_object(g, &c, spl::DEFAULT_VALUE, budget)?;
                let value_type = match single_object_ref(g, &c, spl::VALUE_TYPE, budget)? {
                    None => None,
                    Some(TermRef::NamedNode(vt)) => Some(check_and_own_iri(vt, budget)?),
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
            if matches!(obj, TermRef::NamedNode(_) | TermRef::BlankNode(_)) {
                stack.push(check_and_own_term(obj, budget)?);
            }
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
                if matches!(sup, TermRef::NamedNode(_) | TermRef::BlankNode(_)) {
                    stack.push(check_and_own_term(sup, budget)?);
                }
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
    check: Option<&CandidateCheck<'_>>,
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
        check,
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
    check: Option<&CandidateCheck<'_>>,
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
        let TemplateDef { body, args } = resolve_template(g, template, budget, cache)?;
        let mut bindings: FxHashMap<String, Option<Term>> = FxHashMap::default();
        for arg in args {
            let supplied = match single_object(g, call, arg.predicate.as_ref(), budget)? {
                None => None,
                Some(v) => {
                    if let Some(var_name) = spin_var_name(g, &v, budget)? {
                        if let Some(outer_val) = outer_bindings.get(&var_name) {
                            // Omitted outer optional argument (`None`) falls through to the
                            // inner argument's default / optional / required checks.
                            if let Some(t) = outer_val {
                                let bytes = term_ref_size_and_depth(t.as_ref(), 0, budget)?;
                                budget.charge_bytes(bytes)?;
                                Some(t.clone())
                            } else {
                                None
                            }
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
                    if let Some(default) = arg.default_value {
                        Some(default)
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
            bindings.insert(arg.var, raw);
        }

        // The body may itself be another template call, SPIN RDF syntax, or sp:text
        if let Some(inner_template) = template_of_call(g, &body, budget, cache)? {
            return expand_inner(
                g,
                extra,
                &body,
                &inner_template,
                check,
                &bindings,
                active_templates,
                depth + 1,
                budget,
                cache,
            );
        }
        if has_rdf_query_structure(g, &body) {
            let known_prefixes = check.map(|c| &c.prefixes);
            let rdf_res =
                serialize_query_with_bindings(g, &body, &bindings, known_prefixes, budget)
                    .and_then(|q| {
                        if let Some(c) = check {
                            c.validate(&q, budget)?;
                        }
                        Ok(q)
                    });
            match rdf_res {
                Ok(q) => return Ok(q),
                Err(rdf_err) => {
                    if !is_limit_or_cycle_error(&rdf_err)
                        && let Some(TermRef::Literal(lit)) =
                            single_object_ref(g, &body, sp::TEXT, budget)?
                    {
                        return substitute_sparql_text(lit.value(), &bindings, budget);
                    }
                    return Err(rdf_err);
                }
            }
        }
        match single_object_ref(g, &body, sp::TEXT, budget)? {
            Some(TermRef::Literal(lit)) => substitute_sparql_text(lit.value(), &bindings, budget),
            Some(other) => bail!(
                "the sp:text of spin:body {body} in SPIN template {template} is {other}, not a string",
            ),
            None => bail!(
                "the spin:body {body} of SPIN template {template} has neither SPIN RDF syntax nor sp:text",
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
/// begins with `PN_CHARS_BASE`). Returns `Ok(end)` if a `:` follows a valid `PN_PREFIX`, or
/// `Err(failed_at)` with the end of the scanned prefix run so the caller never rescans it.
fn try_scan_prefixed_name(text: &str, start: usize) -> std::result::Result<usize, usize> {
    let b = text.as_bytes();
    let mut j = start;
    let Some(first) = text[j..].chars().next() else {
        return Err(j);
    };
    if !pn_chars_base(first) {
        return Err(j);
    }
    j += first.len_utf8();
    let mut last_non_dot = j;
    while j < b.len() {
        let Some(c) = text[j..].chars().next() else {
            break;
        };
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
        Ok(scan_pn_local(text, j + 1))
    } else {
        Err(j)
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

/// Tokenize a SPARQL 1.1/1.2 query string into non-whitespace, non-comment tokens in a
/// single forward-only O(n) pass while preserving exact byte offsets and charging `budget`.
fn tokenize_sparql(text: &str, budget: &mut WorkBudget) -> Result<Vec<Tok>> {
    let mut tokens = Vec::new();
    let b = text.as_bytes();
    let mut i = 0usize;
    let mut no_pname_before = 0usize;
    let mut next_byte_charge = 256usize;

    while i < b.len() {
        while i >= next_byte_charge {
            budget.step()?;
            next_byte_charge = next_byte_charge.saturating_add(256);
        }
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i] == b'#' {
            while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        budget.step()?;
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
                        } else if b[j..].starts_with(&[q, q, q]) {
                            // SPARQL STRING_LITERAL_LONG1/2 closes at the first unescaped
                            // triple-quote delimiter (any quote inside before closing must
                            // be followed by a non-quote char or escaped as `\"` / `\'`).
                            j += 3;
                            break;
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
                    if c == b'\\' {
                        if b.get(j + 1) == Some(&b'u')
                            && b.get(j + 2..j + 6)
                                .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
                        {
                            j += 6;
                            continue;
                        }
                        if b.get(j + 1) == Some(&b'U')
                            && b.get(j + 2..j + 10)
                                .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
                        {
                            j += 10;
                            continue;
                        }
                        break;
                    }
                    if c == b'>'
                        || c <= b' '
                        || matches!(c, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`')
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
                    if i >= no_pname_before {
                        match try_scan_prefixed_name(text, i) {
                            Ok(end) => {
                                i = end;
                                tokens.push(Tok {
                                    kind: TokKind::PrefixedName,
                                    start,
                                    end: i,
                                });
                                continue;
                            }
                            Err(failed_at) => {
                                no_pname_before = failed_at;
                            }
                        }
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
    while i >= next_byte_charge {
        budget.step()?;
        next_byte_charge = next_byte_charge.saturating_add(256);
    }
    Ok(tokens)
}

/// Collect every SPARQL variable name (`?name` / `$name`) referenced outside comments,
/// string literals, IRIs, and prefixed names in `text`, charging `budget`.
pub(crate) fn collect_sparql_text_vars(
    text: &str,
    out: &mut FxHashSet<String>,
    budget: &mut WorkBudget,
) -> Result<()> {
    for tok in tokenize_sparql(text, budget)? {
        if let Some(name) = tok.var_name(text)
            && !out.contains(name)
        {
            budget.charge_bytes(name.len())?;
            out.insert(name.to_string());
        }
    }
    Ok(())
}

const PLACEHOLDER_PREFIX_BASE: &str = "urn:sparkles:spin-subquery-check:";

fn indexed_placeholder_ns(idx: usize) -> String {
    format!("{PLACEHOLDER_PREFIX_BASE}p{idx}/")
}

/// Whether `ns` is a prefix of any custom aggregate IRI registered by
/// [`sparkles_core::sparql::aggext::register`]. Every such namespace is short (<= 63 bytes),
/// so emitting the real `PREFIX` declaration for it in subquery preflight cannot cause
/// long-namespace IRI amplification.
fn could_resolve_to_registered_aggregate(ns: &str) -> bool {
    !ns.is_empty()
        && (spargebra::algebra::BUILT_IN_AGGREGATE_IRIS
            .iter()
            .any(|iri| iri.starts_with(ns))
            || spargebra::algebra::ARQ_AGGREGATE_KEYWORDS
                .iter()
                .any(|(_, local)| {
                    let base = spargebra::algebra::ARQ_AGGREGATE_NAMESPACE;
                    let full_len = base.len() + local.len();
                    ns.len() <= full_len
                        && if ns.len() <= base.len() {
                            base.starts_with(ns)
                        } else {
                            ns.starts_with(base) && local.starts_with(&ns[base.len()..])
                        }
                }))
}

fn has_iri_scheme(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    for b in bytes {
        if b == b':' {
            return true;
        }
        if !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
            return false;
        }
    }
    false
}

fn resolved_iri_len(raw: &str, base_len: usize) -> usize {
    if base_len > 0 && !has_iri_scheme(raw) {
        base_len.saturating_add(raw.len())
    } else {
        raw.len()
    }
}

/// Admit the resolved IRI bytes that `spargebra` will materialize when parsing `sparql`
/// under `base_prefixes` (plus any inline `BASE` / `PREFIX` declarations in `sparql`).
/// Refuses with [`super::rdf::SpinLimitError`] before parse allocations if any single
/// resolved IRI exceeds [`MAX_OUTPUT_BYTES`] or total resolved IRI bytes exceed
/// `max_resolved_bytes`.
pub(crate) fn admit_resolved_prefixed_iris(
    sparql: &str,
    base_prefixes: Option<&FxHashMap<&str, &str>>,
    max_resolved_bytes: usize,
) -> Result<()> {
    let mut scan_budget = WorkBudget::new(MAX_OUTPUT_BYTES);
    let tokens = tokenize_sparql(sparql, &mut scan_budget)?;
    let mut base_len = 0usize;
    let mut local_prefixes: FxHashMap<&str, usize> = FxHashMap::default();
    let mut total_resolved = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok.is_word(sparql, "PREFIX")
            && i + 2 < tokens.len()
            && tokens[i + 1].kind == TokKind::PrefixedName
            && tokens[i + 2].kind == TokKind::IriRef
        {
            let p_str = tokens[i + 1].as_str(sparql);
            if let Some(prefix) = p_str.strip_suffix(':') {
                let iri_tok = tokens[i + 2].as_str(sparql);
                let inner = &iri_tok[1..iri_tok.len().saturating_sub(1)];
                let ns_len = resolved_iri_len(inner, base_len);
                if ns_len > MAX_OUTPUT_BYTES {
                    return Err(limit_err(format!(
                        "SPIN resolved IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                    )));
                }
                local_prefixes.insert(prefix, ns_len);
                i += 3;
                continue;
            }
        }
        if tok.is_word(sparql, "BASE")
            && i + 1 < tokens.len()
            && tokens[i + 1].kind == TokKind::IriRef
        {
            let iri_tok = tokens[i + 1].as_str(sparql);
            let inner = &iri_tok[1..iri_tok.len().saturating_sub(1)];
            base_len = resolved_iri_len(inner, base_len);
            if base_len > MAX_OUTPUT_BYTES {
                return Err(limit_err(format!(
                    "SPIN resolved IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                )));
            }
            i += 2;
            continue;
        }
        match tok.kind {
            TokKind::PrefixedName => {
                if let Some((prefix, local)) = tok.as_str(sparql).split_once(':') {
                    let ns_len = local_prefixes
                        .get(prefix)
                        .copied()
                        .or_else(|| base_prefixes.and_then(|m| m.get(prefix).map(|ns| ns.len())));
                    if let Some(ns_len) = ns_len {
                        let resolved_len = ns_len.saturating_add(local.len());
                        if resolved_len > MAX_OUTPUT_BYTES {
                            return Err(limit_err(format!(
                                "SPIN resolved IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                            )));
                        }
                        total_resolved = total_resolved.saturating_add(resolved_len);
                        if total_resolved > max_resolved_bytes {
                            return Err(limit_err(format!(
                                "SPIN query exceeds maximum resolved IRI size ({max_resolved_bytes} bytes)"
                            )));
                        }
                    }
                }
            }
            TokKind::IriRef if base_len > 0 => {
                let iri_tok = tok.as_str(sparql);
                let inner = &iri_tok[1..iri_tok.len().saturating_sub(1)];
                if !has_iri_scheme(inner) {
                    let resolved_len = base_len.saturating_add(inner.len());
                    if resolved_len > MAX_OUTPUT_BYTES {
                        return Err(limit_err(format!(
                            "SPIN resolved IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                        )));
                    }
                    total_resolved = total_resolved.saturating_add(resolved_len);
                    if total_resolved > max_resolved_bytes {
                        return Err(limit_err(format!(
                            "SPIN query exceeds maximum resolved IRI size ({max_resolved_bytes} bytes)"
                        )));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

fn unescape_pn_local(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.next()
        {
            out.push(next);
        } else {
            out.push(c);
        }
    }
    out
}

/// Validate that `text` represents a single valid SPARQL `SubSelect` (starting with `SELECT`,
/// balanced `{}` braces that cannot escape the subquery block, and no dataset clauses or
/// prologue), charging `budget`.
///
/// When `known_prefixes` is `Some`, only declared prefixes are emitted; namespaces that can
/// resolve to a registered custom aggregate keep their real (short) IRI while other declared
/// prefixes use fixed-length indexed placeholder namespaces so discarded structural candidates
/// never amplify long caller namespaces during preflight. When `None` (standalone
/// serialization without a prefix table), indexed placeholder namespaces are emitted for all
/// prefixes and both `DISTINCT` and non-`DISTINCT` single-argument placeholder calls are
/// admitted as potential custom aggregates so missing prefix declarations do not reject
/// valid standalone fragments.
pub(crate) fn validate_subquery_text(
    text: &str,
    known_prefixes: Option<&FxHashMap<&str, &str>>,
    budget: &mut WorkBudget,
) -> Result<()> {
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "SPIN sp:text exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    let tokens = tokenize_sparql(text, budget)?;
    let Some(first) = tokens.first() else {
        bail!("sp:SubQuery sp:text is empty");
    };
    if !first.is_word(text, "SELECT") {
        bail!("sp:SubQuery sp:text must be a SELECT query");
    }
    let mut brace_depth: isize = 0;
    let mut prefix_indices: FxHashMap<&str, usize> = FxHashMap::default();
    let mut used_prefixes: Vec<(&str, String)> = Vec::new();
    let mut distinct_agg_tokens: Vec<(usize, &str)> = Vec::new();
    let mut resolved_iri_bytes = 0usize;
    let max_resolved_bytes = budget.remaining_bytes();
    for (idx, tok) in tokens.iter().enumerate() {
        match tok.kind {
            TokKind::LBrace => brace_depth += 1,
            TokKind::RBrace => {
                brace_depth -= 1;
                if brace_depth < 0 {
                    bail!("sp:SubQuery sp:text has unmatched closing brace");
                }
            }
            TokKind::PrefixedName => {
                if let Some((prefix, local)) = tok.as_str(text).split_once(':') {
                    let slot = if let Some(&existing) = prefix_indices.get(prefix) {
                        Some(existing)
                    } else {
                        let effective_ns = match known_prefixes {
                            Some(prefix_map) => prefix_map.get(prefix).map(|&ns| {
                                if could_resolve_to_registered_aggregate(ns) {
                                    ns.to_string()
                                } else {
                                    indexed_placeholder_ns(used_prefixes.len())
                                }
                            }),
                            None => Some(indexed_placeholder_ns(used_prefixes.len())),
                        };
                        if let Some(ns) = effective_ns {
                            let new_slot = used_prefixes.len();
                            prefix_indices.insert(prefix, new_slot);
                            used_prefixes.push((prefix, ns));
                            Some(new_slot)
                        } else {
                            None
                        }
                    };
                    if let Some(slot) = slot {
                        let resolved_len = used_prefixes[slot].1.len().saturating_add(local.len());
                        if resolved_len > MAX_OUTPUT_BYTES {
                            return Err(limit_err(format!(
                                "SPIN resolved IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                            )));
                        }
                        resolved_iri_bytes = resolved_iri_bytes.saturating_add(resolved_len);
                        if resolved_iri_bytes > max_resolved_bytes {
                            return Err(limit_err(format!(
                                "SPIN query exceeds maximum resolved IRI size ({max_resolved_bytes} bytes)"
                            )));
                        }
                        if known_prefixes.is_none()
                            && tokens
                                .get(idx + 1)
                                .is_some_and(|t| t.kind == TokKind::LParen)
                            && tokens
                                .get(idx + 2)
                                .is_some_and(|t| t.is_word(text, "DISTINCT"))
                        {
                            distinct_agg_tokens.push((slot, local));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if brace_depth != 0 {
        bail!("sp:SubQuery sp:text has unbalanced braces");
    }

    let mut header_bytes = 0usize;
    for (p, ns) in &used_prefixes {
        // "PREFIX " (7) + p + ": <" (3) + ns + ">\n" (2)
        header_bytes = header_bytes
            .saturating_add(12)
            .saturating_add(p.len())
            .saturating_add(ns.len());
    }
    let wrapped_len = header_bytes
        .saturating_add(text.len())
        .saturating_add("SELECT * WHERE { {\n\n} }".len());
    if header_bytes > MAX_OUTPUT_BYTES || wrapped_len > super::rdf::MAX_RETAINED_BYTES {
        return Err(limit_err(format!(
            "SPIN subquery validation wrapper exceeds maximum size ({} bytes)",
            super::rdf::MAX_RETAINED_BYTES
        )));
    }

    let mut wrapped = String::with_capacity(wrapped_len);
    for (p, ns) in &used_prefixes {
        wrapped.push_str("PREFIX ");
        wrapped.push_str(p);
        wrapped.push_str(": <");
        wrapped.push_str(ns);
        wrapped.push_str(">\n");
    }
    wrapped.push_str("SELECT * WHERE { {\n");
    wrapped.push_str(text);
    wrapped.push_str("\n} }");

    let mut parser = sparkles_core::sparql::aggext::register(spargebra::SparqlParser::new());
    if known_prefixes.is_none() {
        parser = parser.with_placeholder_aggregate_namespace(PLACEHOLDER_PREFIX_BASE);
        let mut seen_aggs: FxHashSet<NamedNode> = FxHashSet::default();
        for (slot, local) in distinct_agg_tokens {
            let iri_str = format!("{}{}", used_prefixes[slot].1, unescape_pn_local(local));
            if let Ok(iri) = NamedNode::new(iri_str)
                && seen_aggs.insert(iri.clone())
            {
                parser = parser.with_custom_aggregate_function(iri);
            }
        }
    }
    let parsed = parser.parse_query(&wrapped).map_err(|e| {
        if e.is_too_deep() {
            limit_err(format!("invalid sp:SubQuery sp:text: {e}"))
        } else {
            anyhow!("invalid sp:SubQuery sp:text: {e}")
        }
    })?;
    let (query, depth) = sparkles_core::sparql::depth::checked_query(parsed)
        .map_err(|e| map_core_parse_error(e, "invalid sp:SubQuery sp:text"))?;
    let pattern = match &query {
        spargebra::Query::Select { pattern, .. }
        | spargebra::Query::Construct { pattern, .. }
        | spargebra::Query::Ask { pattern, .. }
        | spargebra::Query::Describe { pattern, .. } => pattern,
    };
    sparkles_core::sparql::depth::with_stack(depth, || {
        sparkles_core::sparql::validate_scoping(pattern)
    })
    .map_err(|e| map_core_parse_error(e, "invalid sp:SubQuery sp:text"))?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VarPosRewrite {
    Plain,
    /// Keep the bare variable `?arg` unchanged because an inner query already exports it.
    KeepVar,
    /// Bare bound variable in `SELECT ?arg` -> `(<term> AS ?arg)`.
    SelectBind,
    /// Bare bound variable in `GROUP BY ?arg` or `ORDER BY ?arg` -> `(<term>)`.
    ParenTerm,
}

fn is_aggregate_keyword(tok: Tok, text: &str) -> bool {
    tok.kind == TokKind::Word
        && (tok.is_word(text, "COUNT")
            || tok.is_word(text, "SUM")
            || tok.is_word(text, "AVG")
            || tok.is_word(text, "MIN")
            || tok.is_word(text, "MAX")
            || tok.is_word(text, "SAMPLE")
            || tok.is_word(text, "GROUP_CONCAT"))
}

fn compute_delimiters(tokens: &[Tok]) -> (Vec<Option<usize>>, Vec<Option<usize>>) {
    let mut matching_rparen = vec![None; tokens.len()];
    let mut matching_rbrace = vec![None; tokens.len()];
    let mut paren_stack = Vec::new();
    let mut brace_stack = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        match tok.kind {
            TokKind::LParen => paren_stack.push(idx),
            TokKind::RParen => {
                if let Some(open) = paren_stack.pop() {
                    matching_rparen[open] = Some(idx);
                }
            }
            TokKind::LBrace => brace_stack.push(idx),
            TokKind::RBrace => {
                if let Some(open) = brace_stack.pop() {
                    matching_rbrace[open] = Some(idx);
                }
            }
            _ => {}
        }
    }
    (matching_rparen, matching_rbrace)
}

struct TextAnalyzer<'a, 'b> {
    text: &'a str,
    tokens: &'a [Tok],
    matching_rparen: Vec<Option<usize>>,
    matching_rbrace: Vec<Option<usize>>,
    bindings: &'a FxHashMap<String, Option<Term>>,
    var_rewrites: &'b mut [VarPosRewrite],
    budget: &'b mut WorkBudget,
}

impl TextAnalyzer<'_, '_> {
    fn is_bound(&self, var: &str) -> bool {
        var != "this" && self.bindings.get(var).is_some_and(Option::is_some)
    }

    fn skip_paren(&self, idx: usize, end: usize) -> usize {
        self.matching_rparen[idx].map_or(end, |close| (close + 1).min(end))
    }

    fn check_values_header(&mut self, values_idx: usize, end: usize) -> Result<usize> {
        let mut k = values_idx + 1;
        if k >= end {
            return Ok(k);
        }
        let next = self.tokens[k];
        if let Some(var) = next.var_name(self.text) {
            if self.is_bound(var) {
                bail!("SPIN template body uses bound argument ?{var} in VALUES");
            }
            return Ok(k + 1);
        }
        if next.kind == TokKind::LParen {
            k += 1;
            while k < end {
                let t = self.tokens[k];
                if t.kind == TokKind::RParen {
                    return Ok(k + 1);
                }
                if matches!(t.kind, TokKind::LBrace | TokKind::RBrace)
                    || t.is_word(self.text, "VALUES")
                {
                    break;
                }
                if let Some(var) = t.var_name(self.text)
                    && self.is_bound(var)
                {
                    bail!("SPIN template body uses bound argument ?{var} in VALUES");
                }
                k += 1;
            }
        }
        Ok(k)
    }

    /// Analyze an expression range for nested `{ ... }` patterns (e.g. `EXISTS { ... }`) and
    /// return whether an aggregate keyword appears at this expression level.
    fn analyze_expr_range(&mut self, start: usize, end: usize, depth: usize) -> Result<bool> {
        if depth > MAX_DEPTH * 2 {
            return Err(limit_err(format!(
                "SPARQL text exceeds maximum nesting depth ({})",
                MAX_DEPTH * 2
            )));
        }
        let mut has_agg = false;
        let mut k = start;
        while k < end {
            let tok = self.tokens[k];
            if tok.kind == TokKind::LBrace {
                let rb = self.matching_rbrace[k].unwrap_or(end).min(end);
                if k + 1 < rb && self.tokens[k + 1].is_word(self.text, "SELECT") {
                    self.analyze_select(k + 1, rb, depth + 1)?;
                } else {
                    self.analyze_group_pattern(k + 1, rb, depth + 1)?;
                }
                k = (rb + 1).min(end);
                continue;
            }
            if is_aggregate_keyword(tok, self.text) {
                has_agg = true;
            }
            k += 1;
        }
        Ok(has_agg)
    }

    fn scan_group_by(
        &mut self,
        start: usize,
        end: usize,
        body_exported: &FxHashSet<String>,
        depth: usize,
    ) -> Result<(usize, FxHashSet<String>)> {
        let mut k = start;
        let mut group_exported = FxHashSet::default();
        while k < end {
            let tok = self.tokens[k];
            match tok.kind {
                TokKind::LParen => {
                    let next_k = self.skip_paren(k, end);
                    let close = next_k.saturating_sub(1);
                    if close > k && self.matching_rparen[k] == Some(close) {
                        let mut inner_l = k + 1;
                        let mut inner_r = close;
                        while inner_l < inner_r
                            && self.tokens[inner_l].kind == TokKind::LParen
                            && self.matching_rparen[inner_l] == Some(inner_r - 1)
                        {
                            inner_l += 1;
                            inner_r -= 1;
                        }
                        if inner_l + 1 == inner_r
                            && self.tokens[inner_l].kind == TokKind::Var
                            && let Some(var) = self.tokens[inner_l].var_name(self.text)
                            && self.is_bound(var)
                            && body_exported.contains(var)
                        {
                            self.var_rewrites[inner_l] = VarPosRewrite::KeepVar;
                            group_exported.insert(var.to_string());
                            k = next_k;
                            continue;
                        }
                    }
                    self.analyze_expr_range(k + 1, close, depth + 1)?;
                    k = next_k;
                }
                TokKind::Var => {
                    if let Some(var) = tok.var_name(self.text)
                        && self.is_bound(var)
                        && body_exported.contains(var)
                    {
                        self.var_rewrites[k] = VarPosRewrite::KeepVar;
                        group_exported.insert(var.to_string());
                    } else {
                        self.var_rewrites[k] = VarPosRewrite::ParenTerm;
                    }
                    k += 1;
                }
                TokKind::LBrace | TokKind::RBrace => break,
                TokKind::Word
                    if tok.is_word(self.text, "GROUP")
                        || tok.is_word(self.text, "HAVING")
                        || tok.is_word(self.text, "ORDER")
                        || tok.is_word(self.text, "LIMIT")
                        || tok.is_word(self.text, "OFFSET")
                        || tok.is_word(self.text, "VALUES")
                        || tok.is_word(self.text, "SELECT")
                        || tok.is_word(self.text, "WHERE") =>
                {
                    break;
                }
                _ => k += 1,
            }
        }
        Ok((k, group_exported))
    }

    fn scan_order_by(&mut self, start: usize, end: usize, depth: usize) -> Result<usize> {
        let mut k = start;
        while k < end {
            let tok = self.tokens[k];
            match tok.kind {
                TokKind::LParen => {
                    let next_k = self.skip_paren(k, end);
                    self.analyze_expr_range(k + 1, next_k.saturating_sub(1), depth + 1)?;
                    k = next_k;
                }
                TokKind::Var => {
                    self.var_rewrites[k] = VarPosRewrite::ParenTerm;
                    k += 1;
                }
                TokKind::LBrace | TokKind::RBrace => break,
                TokKind::Word
                    if tok.is_word(self.text, "ORDER")
                        || tok.is_word(self.text, "GROUP")
                        || tok.is_word(self.text, "HAVING")
                        || tok.is_word(self.text, "LIMIT")
                        || tok.is_word(self.text, "OFFSET")
                        || tok.is_word(self.text, "VALUES")
                        || tok.is_word(self.text, "SELECT")
                        || tok.is_word(self.text, "WHERE") =>
                {
                    break;
                }
                _ => k += 1,
            }
        }
        Ok(k)
    }

    fn analyze_select(
        &mut self,
        sel_idx: usize,
        end: usize,
        depth: usize,
    ) -> Result<FxHashSet<String>> {
        if depth > MAX_DEPTH * 2 {
            return Err(limit_err(format!(
                "SPARQL text exceeds maximum nesting depth ({})",
                MAX_DEPTH * 2
            )));
        }
        self.budget.step()?;
        let mut k = sel_idx + 1;
        if k < end
            && (self.tokens[k].is_word(self.text, "DISTINCT")
                || self.tokens[k].is_word(self.text, "REDUCED"))
        {
            k += 1;
        }
        let mut is_star = false;
        let mut proj_vars: Vec<(usize, &str)> = Vec::new();
        let mut has_agg = false;
        if k < end
            && self.tokens[k].kind == TokKind::Other
            && self.tokens[k].as_str(self.text) == "*"
        {
            is_star = true;
            k += 1;
        } else {
            while k < end {
                match self.tokens[k].kind {
                    TokKind::LParen => {
                        let next_k = self.skip_paren(k, end);
                        if self.analyze_expr_range(k + 1, next_k.saturating_sub(1), depth + 1)? {
                            has_agg = true;
                        }
                        k = next_k;
                    }
                    TokKind::Var => {
                        if let Some(var) = self.tokens[k].var_name(self.text) {
                            proj_vars.push((k, var));
                        }
                        k += 1;
                    }
                    _ => break,
                }
            }
        }

        // Skip optional FROM / FROM NAMED dataset clauses and optional WHERE keyword
        while k < end {
            let tok = self.tokens[k];
            if tok.kind == TokKind::LBrace {
                break;
            }
            if tok.is_word(self.text, "WHERE")
                || tok.is_word(self.text, "FROM")
                || tok.is_word(self.text, "NAMED")
                || matches!(
                    tok.kind,
                    TokKind::IriRef | TokKind::PrefixedName | TokKind::Var
                )
            {
                k += 1;
            } else {
                break;
            }
        }

        let mut body_exported = FxHashSet::default();
        if k < end && self.tokens[k].kind == TokKind::LBrace {
            let rb = self.matching_rbrace[k].unwrap_or(end).min(end);
            body_exported = self.analyze_group_pattern(k + 1, rb, depth + 1)?;
            k = (rb + 1).min(end);
        }

        // Solution modifiers trailing this SELECT
        let mut has_group_by = false;
        let mut group_exported = FxHashSet::default();
        while k < end {
            let tok = self.tokens[k];
            if tok.is_word(self.text, "GROUP")
                && k + 1 < end
                && self.tokens[k + 1].is_word(self.text, "BY")
            {
                has_group_by = true;
                let (next_k, grp) = self.scan_group_by(k + 2, end, &body_exported, depth)?;
                group_exported.extend(grp);
                k = next_k;
                continue;
            }
            if tok.is_word(self.text, "HAVING") {
                let mut h = k + 1;
                while h < end {
                    let ht = self.tokens[h];
                    if ht.kind == TokKind::LParen {
                        let next_h = self.skip_paren(h, end);
                        if self.analyze_expr_range(h + 1, next_h.saturating_sub(1), depth + 1)? {
                            has_agg = true;
                        }
                        h = next_h;
                        continue;
                    }
                    if matches!(ht.kind, TokKind::LBrace | TokKind::RBrace)
                        || ht.is_word(self.text, "ORDER")
                        || ht.is_word(self.text, "LIMIT")
                        || ht.is_word(self.text, "OFFSET")
                        || ht.is_word(self.text, "VALUES")
                        || ht.is_word(self.text, "GROUP")
                        || ht.is_word(self.text, "HAVING")
                        || ht.is_word(self.text, "SELECT")
                    {
                        break;
                    }
                    if is_aggregate_keyword(ht, self.text) {
                        has_agg = true;
                    }
                    h += 1;
                }
                k = h;
                continue;
            }
            if tok.is_word(self.text, "ORDER")
                && k + 1 < end
                && self.tokens[k + 1].is_word(self.text, "BY")
            {
                k = self.scan_order_by(k + 2, end, depth)?;
                continue;
            }
            if tok.is_word(self.text, "VALUES") {
                k = self.check_values_header(k, end)?;
                continue;
            }
            if tok.kind == TokKind::LBrace {
                let rb = self.matching_rbrace[k].unwrap_or(end).min(end);
                if k + 1 < rb && self.tokens[k + 1].is_word(self.text, "SELECT") {
                    self.analyze_select(k + 1, rb, depth + 1)?;
                } else {
                    self.analyze_group_pattern(k + 1, rb, depth + 1)?;
                }
                k = (rb + 1).min(end);
                continue;
            }
            k += 1;
        }

        let visible_at_select = if has_group_by {
            group_exported
        } else if has_agg {
            FxHashSet::default()
        } else {
            body_exported
        };

        if is_star {
            return Ok(visible_at_select);
        }
        let mut select_exported = FxHashSet::default();
        for (tok_idx, var) in proj_vars {
            if self.is_bound(var) {
                if visible_at_select.contains(var) {
                    self.var_rewrites[tok_idx] = VarPosRewrite::KeepVar;
                } else {
                    self.var_rewrites[tok_idx] = VarPosRewrite::SelectBind;
                }
                select_exported.insert(var.to_string());
            }
        }
        Ok(select_exported)
    }

    fn analyze_group_pattern(
        &mut self,
        start: usize,
        end: usize,
        depth: usize,
    ) -> Result<FxHashSet<String>> {
        if depth > MAX_DEPTH * 2 {
            return Err(limit_err(format!(
                "SPARQL text exceeds maximum nesting depth ({})",
                MAX_DEPTH * 2
            )));
        }
        let mut exported = FxHashSet::default();
        let mut i = start;
        while i < end {
            let tok = self.tokens[i];
            if (tok.is_word(self.text, "MINUS") || tok.is_word(self.text, "EXISTS"))
                && i + 1 < end
                && self.tokens[i + 1].kind == TokKind::LBrace
            {
                let lb = i + 1;
                let rb = self.matching_rbrace[lb].unwrap_or(end).min(end);
                self.analyze_group_pattern(lb + 1, rb, depth + 1)?;
                i = (rb + 1).min(end);
                continue;
            }
            if tok.kind == TokKind::LBrace {
                let rb = self.matching_rbrace[i].unwrap_or(end).min(end);
                if i + 1 < rb && self.tokens[i + 1].is_word(self.text, "SELECT") {
                    let sub_vars = self.analyze_select(i + 1, rb, depth + 1)?;
                    exported.extend(sub_vars);
                } else {
                    let sub_vars = self.analyze_group_pattern(i + 1, rb, depth + 1)?;
                    exported.extend(sub_vars);
                }
                i = (rb + 1).min(end);
                continue;
            }
            if tok.is_word(self.text, "SELECT") {
                let sub_vars = self.analyze_select(i, end, depth + 1)?;
                exported.extend(sub_vars);
                break;
            }
            if tok.is_word(self.text, "GROUP")
                && i + 1 < end
                && self.tokens[i + 1].is_word(self.text, "BY")
            {
                let (next_i, _) = self.scan_group_by(i + 2, end, &exported, depth)?;
                i = next_i;
                continue;
            }
            if tok.is_word(self.text, "ORDER")
                && i + 1 < end
                && self.tokens[i + 1].is_word(self.text, "BY")
            {
                i = self.scan_order_by(i + 2, end, depth)?;
                continue;
            }
            if tok.is_word(self.text, "VALUES") {
                i = self.check_values_header(i, end)?;
                continue;
            }
            i += 1;
        }
        Ok(exported)
    }
}

/// Token-aware substitution of template argument bindings into a SPARQL `sp:text` query,
/// returning both the rewritten SPARQL text and the set of bound template variables exported
/// by its top-level `SELECT` (used when embedding a textual `sp:SubQuery` inside an RDF query).
pub(crate) fn substitute_sparql_text_with_exported(
    text: &str,
    bindings: &FxHashMap<String, Option<Term>>,
    budget: &mut WorkBudget,
) -> Result<(String, FxHashSet<String>)> {
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "SPIN sp:text exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    let tokens = tokenize_sparql(text, budget)?;
    let (matching_rparen, matching_rbrace) = compute_delimiters(&tokens);
    let mut var_rewrites = vec![VarPosRewrite::Plain; tokens.len()];
    let exported = {
        let mut analyzer = TextAnalyzer {
            text,
            tokens: &tokens,
            matching_rparen,
            matching_rbrace,
            bindings,
            var_rewrites: &mut var_rewrites,
            budget,
        };
        analyzer.analyze_group_pattern(0, tokens.len(), 0)?
    };

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
            push_str_bounded(&mut out, "(true)")?;
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
            match var_rewrites[i] {
                VarPosRewrite::KeepVar => {}
                VarPosRewrite::SelectBind => {
                    push_str_bounded(&mut out, &text[copied..tok.start])?;
                    push_str_bounded(&mut out, "(")?;
                    write_term_value(&mut out, term, 0)?;
                    push_str_bounded(&mut out, " AS ?")?;
                    push_str_bounded(&mut out, var)?;
                    push_str_bounded(&mut out, ")")?;
                    copied = tok.end;
                }
                VarPosRewrite::ParenTerm => {
                    push_str_bounded(&mut out, &text[copied..tok.start])?;
                    push_str_bounded(&mut out, "(")?;
                    write_term_value(&mut out, term, 0)?;
                    push_str_bounded(&mut out, ")")?;
                    copied = tok.end;
                }
                VarPosRewrite::Plain => {
                    push_str_bounded(&mut out, &text[copied..tok.start])?;
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
                    copied = tok.end;
                }
            }
        }
        i += 1;
    }
    push_str_bounded(&mut out, &text[copied..])?;
    Ok((out, exported))
}

/// Token-aware substitution of template argument bindings into a SPARQL `sp:text` query.
///
/// - Tokenizes string literals (`'...'`, `"..."`, `'''...'''`, `"""..."""`), IRI references
///   (`<...>` with `\uXXXX`/`\UXXXXXXXX` escapes), prefixed names (`PNAME_NS`/`PNAME_LN` with
///   `-`/`.`/`PLX`), blank node labels, language tags, and `#` comments (CR/LF terminated)
///   in a single forward-only O(n) pass under `budget`.
/// - Rewrites `BOUND(?arg)` / `BOUND($arg)` to `(true)` when `?arg` is bound, valid in both
///   expression and constraint positions (`FILTER`, `HAVING`, `GROUP BY`, `ORDER BY`).
/// - Refuses `AS ?arg` and `VALUES ... ?arg` headers when `?arg` is bound.
/// - Preserves valid SPARQL syntax and scope in `SELECT ?arg` (keeping `?arg` when an inner
///   query already exports it, otherwise `(<val> AS ?arg)`), `GROUP BY ?arg` / `(?arg)`, and
///   `ORDER BY ?arg`.
/// - Avoids merging `<` with a substituted `<iri>` into RDF-star `<<` (or `>` with `>`).
/// - Never substitutes the reserved focus variable `?this`.
pub(crate) fn substitute_sparql_text(
    text: &str,
    bindings: &FxHashMap<String, Option<Term>>,
    budget: &mut WorkBudget,
) -> Result<String> {
    substitute_sparql_text_with_exported(text, bindings, budget).map(|(out, _)| out)
}
