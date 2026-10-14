//! The constraints layer of a schema report ([`sparkles_core::schema::constraints`]) from
//! the schema of a ShEx write-time guard.
//!
//! A class gets the shapes that the guard's shape map selects with `{FOCUS rdf:type C}`.
//! Each triple constraint of such a shape (and of the shapes it extends) is a property
//! constraint with its predicate, or its inverse, and its cardinality. A node constraint
//! gives the datatype, the node kind or the value set, and a reference to another shape
//! gives its class when exactly one class selects that shape. A triple constraint inside
//! a group that is not a plain sequence (a choice, or a group with a cardinality) is
//! listed by its predicate with the group named under `other`, since its cardinality is
//! not the shape's. Whatever else a constraint says (facets, disjunctions, nested shapes,
//! semantic actions) goes under `other` as well, by the SHACL component that checks the
//! same thing where there is one, and by a `http://www.w3.org/ns/shex#` name otherwise.

use crate::NodeSelector;
use crate::ast::{NodeConstraint, NodeKind, ObjectLiteral, ObjectValue, ValueSetValue};
use crate::guard::ShexGuard;
use crate::ir::{Dir, Ir, PairKind, Se, SeId, ShapeId, ShapeIr, Te, TeId};
use oxrdf::{Literal, NamedNode, Term};
use rustc_hash::FxHashSet;
use sparkles_core::guard::GuardMode;
use sparkles_core::schema::constraints::{
    ClassConstraints, ConstraintSource, Enforcement, PropertyConstraint, SourceKind,
};
use std::collections::{BTreeMap, BTreeSet};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SH: &str = "http://www.w3.org/ns/shacl#";
const SHEX: &str = "http://www.w3.org/ns/shex#";

/// The source of the shapes an installed ShEx guard validates every write against.
pub fn guard_source(guard: &ShexGuard) -> ConstraintSource {
    let model = guard.model();
    let cfg = guard.config();
    let ir = model.schema.ir();
    let enforcement = match cfg.mode {
        GuardMode::Reject => Enforcement::RejectOnWrite,
        GuardMode::Warn => Enforcement::WarnOnWrite,
        GuardMode::Off => Enforcement::ValidatedOnRequest,
    };
    // the classes the shape map selects, per shape label
    let mut by_label: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut other_targets = 0usize;
    for a in &model.map.0 {
        let class = match &a.node {
            NodeSelector::Focus {
                subject: None,
                predicate,
                object: Some(Term::NamedNode(c)),
                focus_is_subject: true,
            } if predicate.as_str() == RDF_TYPE => Some(c.as_str().to_string()),
            _ => None,
        };
        match (class, a.shape.as_shexj()) {
            (Some(c), Some(l)) => {
                by_label.entry(l).or_default().insert(c);
            }
            _ => other_targets += 1,
        }
    }
    let mut classes: BTreeMap<String, ClassAcc> = BTreeMap::new();
    for (label, selected) in &by_label {
        let Some(pk) = model.schema.label(label) else {
            continue;
        };
        let mut shapes = Vec::new();
        let mut seen = FxHashSet::default();
        collect_shapes(ir, ir.pairs[pk.index()].se, &mut shapes, &mut seen, 0);
        for class in selected {
            let acc = classes.entry(class.clone()).or_default();
            acc.labels.insert(label.clone());
            for &s in &shapes {
                acc.add_shape(ir, s, &by_label, enforcement);
            }
        }
    }
    let classes = classes
        .into_iter()
        .map(|(class, acc)| {
            let mut properties = acc.properties;
            properties.sort_by(|a, b| {
                (&a.path, a.inverse, a.enforcement).cmp(&(&b.path, b.inverse, b.enforcement))
            });
            properties.dedup();
            ClassConstraints {
                class,
                shapes: acc
                    .labels
                    .into_iter()
                    .filter(|l| !l.starts_with("_:"))
                    .collect(),
                closed: acc.closed,
                properties,
                other_paths: 0,
            }
        })
        .collect();
    ConstraintSource {
        kind: SourceKind::Guard,
        graphs: cfg.schema.graphs.clone().unwrap_or_default(),
        file: cfg.schema.graphs.is_none(),
        mode: Some(
            match cfg.mode {
                GuardMode::Reject => "reject",
                GuardMode::Warn => "warn",
                GuardMode::Off => "off",
            }
            .to_string(),
        ),
        threshold: None,
        shapes: ir.shapes.len(),
        other_targets,
        classes,
    }
}

#[derive(Default)]
struct ClassAcc {
    labels: BTreeSet<String>,
    closed: bool,
    properties: Vec<PropertyConstraint>,
}

impl ClassAcc {
    fn add_shape(
        &mut self,
        ir: &Ir,
        s: ShapeId,
        by_label: &BTreeMap<String, BTreeSet<String>>,
        enforcement: Enforcement,
    ) {
        let shape = &ir.shapes[s.index()];
        self.closed |= shape.closed;
        // a shape that extends others also needs their triple constraints
        let mut all = vec![s];
        if let Some(ext) = &shape.ext
            && let Some(frame) = ext.frames.first()
        {
            all.extend(frame.parts.iter().map(|p| p.shape));
        }
        for id in all {
            let shape = &ir.shapes[id.index()];
            self.closed |= shape.closed;
            let mut plain = vec![false; shape.tcs.len()];
            if let Some(root) = shape.root {
                mark_plain(shape, root, true, &mut plain);
            }
            for (i, tc) in shape.tcs.iter().enumerate() {
                self.properties.push(property(
                    ir,
                    tc,
                    plain.get(i).copied().unwrap_or(false),
                    by_label,
                    enforcement,
                ));
            }
        }
    }
}

/// The shapes a shape expression requires of a node: the shapes it is, and those of
/// the declared shapes it refers to and conjoins. A disjunction or a negation requires
/// none in particular.
fn collect_shapes(
    ir: &Ir,
    se: SeId,
    out: &mut Vec<ShapeId>,
    seen: &mut FxHashSet<usize>,
    depth: usize,
) {
    if depth > 64 || !seen.insert(se.index()) {
        return;
    }
    match &ir.ses[se.index()] {
        Se::Shape(s) => out.push(*s),
        Se::And(xs) => {
            for x in xs {
                collect_shapes(ir, *x, out, seen, depth + 1);
            }
        }
        Se::Ref(pk) => collect_shapes(ir, ir.pairs[pk.index()].se, out, seen, depth + 1),
        Se::Or(_) | Se::Not(_) | Se::Nc(_) | Se::External => {}
    }
}

/// Mark the triple constraints that are required as a plain sequence from the root: not
/// under a choice and not under a group with a cardinality of its own.
fn mark_plain(shape: &ShapeIr, te: TeId, plain: bool, out: &mut [bool]) {
    match &shape.te[te.index()] {
        Te::Tc(tc) => {
            if let Some(slot) = out.get_mut(tc.index()) {
                *slot = plain;
            }
        }
        Te::EachOf { kids, min, max, .. } => {
            let exact = plain && *min == 1 && *max == Some(1);
            for k in kids {
                mark_plain(shape, *k, exact, out);
            }
        }
        Te::OneOf { kids, .. } => {
            for k in kids {
                mark_plain(shape, *k, false, out);
            }
        }
    }
}

fn component(name: &str) -> String {
    format!("{SH}{name}ConstraintComponent")
}

fn shex(name: &str) -> String {
    format!("{SHEX}{name}")
}

/// A value-set value as a term in N-Triples syntax, if it is a plain value (not a stem
/// or a range).
fn value_term(v: &ValueSetValue) -> Option<String> {
    match v {
        ValueSetValue::Object(ObjectValue::Iri(i)) => NamedNode::new(i.as_str())
            .ok()
            .map(|n| Term::from(n).to_string()),
        ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
            value,
            language,
            datatype,
        })) => {
            let l = match (language, datatype) {
                (Some(lang), _) => {
                    Literal::new_language_tagged_literal(value.as_str(), lang.as_str()).ok()?
                }
                (None, Some(dt)) => {
                    Literal::new_typed_literal(value.as_str(), NamedNode::new(dt.as_str()).ok()?)
                }
                (None, None) => Literal::new_simple_literal(value.as_str()),
            };
            Some(Term::from(l).to_string())
        }
        _ => None,
    }
}

fn property(
    ir: &Ir,
    tc: &crate::ir::TcIr,
    plain: bool,
    by_label: &BTreeMap<String, BTreeSet<String>>,
    enforcement: Enforcement,
) -> PropertyConstraint {
    let mut p = PropertyConstraint {
        path: tc.pred.clone(),
        inverse: tc.dir == Dir::In,
        shape: None,
        severity: format!("{SH}Violation"),
        enforcement,
        min_count: None,
        max_count: None,
        datatype: None,
        class: Vec::new(),
        node_kind: None,
        values: Vec::new(),
        other: Vec::new(),
    };
    let mut other = BTreeSet::new();
    if plain {
        if tc.min > 0 {
            p.min_count = Some(u64::from(tc.min));
        }
        p.max_count = tc.max.map(u64::from);
    } else {
        other.insert(shex("Group"));
    }
    if !tc.sem_acts.is_empty() {
        other.insert(shex("SemanticAction"));
    }
    if let Some(v) = tc.value {
        value(ir, v, &mut p, &mut other, by_label, 0);
    }
    p.class.sort();
    p.class.dedup();
    p.other = other.into_iter().collect();
    p
}

/// What a value expression says of the values of a triple constraint.
fn value(
    ir: &Ir,
    se: SeId,
    p: &mut PropertyConstraint,
    other: &mut BTreeSet<String>,
    by_label: &BTreeMap<String, BTreeSet<String>>,
    depth: usize,
) {
    if depth > 16 {
        other.insert(shex("ShapeExpression"));
        return;
    }
    match &ir.ses[se.index()] {
        Se::And(xs) => {
            for x in xs {
                value(ir, *x, p, other, by_label, depth + 1);
            }
        }
        Se::Or(_) => {
            other.insert(shex("OR"));
        }
        Se::Not(_) => {
            other.insert(shex("NOT"));
        }
        Se::Shape(_) => {
            other.insert(shex("Shape"));
        }
        Se::External => {
            other.insert(shex("External"));
        }
        Se::Ref(pk) => reference(ir, *pk, p, other, by_label),
        Se::Nc(nc) => node_constraint(&ir.ncs[nc.index()].nc, p, other),
    }
}

/// A reference to a declared shape: its class when one class selects it.
fn reference(
    ir: &Ir,
    pk: PairKind,
    p: &mut PropertyConstraint,
    other: &mut BTreeSet<String>,
    by_label: &BTreeMap<String, BTreeSet<String>>,
) {
    let label = ir.pairs[pk.index()].label.as_ref().map(|l| l.to_shexj());
    match label.and_then(|l| by_label.get(&l)) {
        Some(classes) if classes.len() == 1 => {
            p.class.extend(classes.iter().cloned());
        }
        _ => {
            other.insert(shex("ShapeReference"));
        }
    }
}

fn node_constraint(nc: &NodeConstraint, p: &mut PropertyConstraint, other: &mut BTreeSet<String>) {
    if let Some(d) = &nc.datatype {
        if p.datatype.is_none() {
            p.datatype = Some(d.clone());
        } else {
            other.insert(component("Datatype"));
        }
    }
    if let Some(k) = nc.node_kind {
        if p.node_kind.is_none() {
            p.node_kind = Some(format!(
                "{SH}{}",
                match k {
                    NodeKind::Iri => "IRI",
                    NodeKind::BNode => "BlankNode",
                    NodeKind::NonLiteral => "BlankNodeOrIRI",
                    NodeKind::Literal => "Literal",
                }
            ));
        } else {
            other.insert(component("NodeKind"));
        }
    }
    if let Some(values) = &nc.values {
        let terms: Option<Vec<String>> = values.iter().map(value_term).collect();
        match terms {
            Some(t) if p.values.is_empty() => p.values = t,
            _ => {
                other.insert(shex("ValueSetStem"));
            }
        }
    }
    if nc.pattern.is_some() {
        other.insert(component("Pattern"));
    }
    if nc.length.is_some() {
        other.insert(component("MinLength"));
        other.insert(component("MaxLength"));
    }
    if nc.min_length.is_some() {
        other.insert(component("MinLength"));
    }
    if nc.max_length.is_some() {
        other.insert(component("MaxLength"));
    }
    for (set, name) in [
        (nc.min_inclusive.is_some(), "MinInclusive"),
        (nc.min_exclusive.is_some(), "MinExclusive"),
        (nc.max_inclusive.is_some(), "MaxInclusive"),
        (nc.max_exclusive.is_some(), "MaxExclusive"),
    ] {
        if set {
            other.insert(component(name));
        }
    }
    if nc.total_digits.is_some() {
        other.insert(shex("TotalDigits"));
    }
    if nc.fraction_digits.is_some() {
        other.insert(shex("FractionDigits"));
    }
}
