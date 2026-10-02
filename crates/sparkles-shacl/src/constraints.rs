//! The constraints layer of a schema report ([`sparkles::schema::constraints`]) from
//! parsed shapes: for each class that shapes target, the property shapes with a
//! predicate path and their `sh:minCount`, `sh:maxCount`, `sh:datatype`, `sh:class` and
//! `sh:nodeKind`.
//!
//! A class's property shapes are those of the shapes that target it, and those reached
//! from them through `sh:node` and `sh:and`, which apply to the same focus nodes.
//! Shapes reached through `sh:or`, `sh:xone`, `sh:not` or qualified value shapes impose
//! nothing on their own and are not listed.

use crate::guard::{ShaclGuard, ValidationConfig, severity_of};
use crate::path::PropertyPath;
use crate::shapes::{Constraint, ShapeId, Shapes, Target};
use anyhow::Result;
use oxrdf::Term;
use rustc_hash::FxHashSet;
use sparkles::guard::{GuardMode, Severity};
use sparkles::schema::constraints::{
    ClassConstraints, ConstraintSource, Enforcement, PropertyConstraint, SourceKind,
};
use sparkles::store::Snapshot;
use std::collections::{BTreeMap, BTreeSet};

/// What checks a set of shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    /// Nothing, until the data is validated on request.
    OnRequest,
    /// Write-time validation with this mode and threshold.
    OnWrite {
        mode: GuardMode,
        threshold: Severity,
    },
}

impl Checked {
    fn enforcement(self, severity: &str) -> Enforcement {
        match self {
            Checked::OnRequest => Enforcement::ValidatedOnRequest,
            Checked::OnWrite {
                mode: GuardMode::Reject,
                threshold,
            } if severity_of(severity) >= threshold => Enforcement::RejectOnWrite,
            Checked::OnWrite {
                mode: GuardMode::Off,
                ..
            } => Enforcement::ValidatedOnRequest,
            Checked::OnWrite { .. } => Enforcement::WarnOnWrite,
        }
    }
}

/// The class constraints of `shapes`, sorted by class IRI, and the number of active
/// shapes whose targets are not classes.
pub fn class_constraints(shapes: &Shapes, checked: Checked) -> (Vec<ClassConstraints>, usize) {
    let all = shapes.shapes();
    let mut by_class: BTreeMap<String, ClassAcc> = BTreeMap::new();
    let mut other_targets = 0usize;
    for (si, shape) in all.iter().enumerate() {
        if shape.deactivated || shape.targets.is_empty() {
            continue;
        }
        let classes: Vec<&str> = shape
            .targets
            .iter()
            .filter_map(|t| match t {
                Target::Class(c) => match shapes.term(*c) {
                    Term::NamedNode(n) => Some(n.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if classes.is_empty() {
            other_targets += 1;
            continue;
        }
        let mut walk = Walk::default();
        walk.shape(shapes, si);
        for c in classes {
            let acc = by_class.entry(c.to_string()).or_default();
            if let Term::NamedNode(n) = &shape.node {
                acc.shapes.insert(n.as_str().to_string());
            }
            acc.closed |= walk.closed;
            acc.property_shapes.extend(walk.properties.iter().copied());
        }
    }
    let classes = by_class
        .into_iter()
        .map(|(class, acc)| {
            let mut properties = Vec::new();
            let mut other_paths = 0usize;
            for ps in acc.property_shapes {
                match property(shapes, ps, checked) {
                    Some(p) => properties.push(p),
                    None => other_paths += 1,
                }
            }
            properties.sort_by(|a, b| {
                (&a.path, &a.shape, a.enforcement, &a.severity).cmp(&(
                    &b.path,
                    &b.shape,
                    b.enforcement,
                    &b.severity,
                ))
            });
            ClassConstraints {
                class,
                shapes: acc.shapes.into_iter().collect(),
                closed: acc.closed,
                properties,
                other_paths,
            }
        })
        .collect();
    (classes, other_targets)
}

#[derive(Default)]
struct ClassAcc {
    shapes: BTreeSet<String>,
    closed: bool,
    /// property shapes, each once
    property_shapes: BTreeSet<ShapeId>,
}

/// The property shapes that apply to the focus nodes of a shape.
#[derive(Default)]
struct Walk {
    visited: FxHashSet<ShapeId>,
    properties: Vec<ShapeId>,
    closed: bool,
}

impl Walk {
    fn shape(&mut self, shapes: &Shapes, si: ShapeId) {
        let shape = &shapes.shapes()[si];
        if shape.deactivated || !self.visited.insert(si) {
            return;
        }
        if shape.is_property_shape() {
            self.properties.push(si);
            return;
        }
        for c in &shape.constraints {
            match c {
                Constraint::Property(ps) => {
                    if !shapes.shapes()[*ps].deactivated && self.visited.insert(*ps) {
                        self.properties.push(*ps);
                    }
                }
                Constraint::Node(n) => self.shape(shapes, *n),
                Constraint::And(list) => {
                    for n in list {
                        self.shape(shapes, *n);
                    }
                }
                Constraint::Closed { .. } => self.closed = true,
                _ => {}
            }
        }
    }
}

/// One property shape, or `None` when its path is not a single predicate.
fn property(shapes: &Shapes, ps: ShapeId, checked: Checked) -> Option<PropertyConstraint> {
    let shape = &shapes.shapes()[ps];
    let Some(PropertyPath::Predicate(path)) = &shape.path else {
        return None;
    };
    let severity = shape.severity.as_str().to_string();
    let mut p = PropertyConstraint {
        path: path.as_str().to_string(),
        shape: match &shape.node {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        },
        enforcement: checked.enforcement(&severity),
        severity,
        min_count: None,
        max_count: None,
        datatype: None,
        class: Vec::new(),
        node_kind: None,
        other: Vec::new(),
    };
    let mut other = BTreeSet::new();
    for c in &shape.constraints {
        match c {
            Constraint::MinCount(n) => p.min_count = Some(p.min_count.map_or(*n, |m| m.max(*n))),
            Constraint::MaxCount(n) => p.max_count = Some(p.max_count.map_or(*n, |m| m.min(*n))),
            Constraint::Datatype(d) if p.datatype.is_none() => {
                p.datatype = Some(d.as_str().to_string());
            }
            Constraint::NodeKind(k) if p.node_kind.is_none() => {
                p.node_kind = Some(k.iri().as_str().to_string());
            }
            Constraint::Class(t) => match shapes.term(*t) {
                Term::NamedNode(n) => p.class.push(n.as_str().to_string()),
                _ => {
                    other.insert(c.component().into_string());
                }
            },
            c => {
                other.insert(c.component().into_string());
            }
        }
    }
    p.class.sort();
    p.class.dedup();
    p.other = other.into_iter().collect();
    Some(p)
}

/// The source of the shapes read from graphs of `snap` (graph IRIs, or `default` for
/// the default graph), validated on request only.
pub fn graphs_source(snap: &Snapshot, graphs: &[String]) -> Result<ConstraintSource> {
    let iris: Vec<String> = graphs
        .iter()
        .map(|g| match g.as_str() {
            "default" => sparkles::sparql::ctx::DEFAULT_GRAPH_IRI.to_string(),
            g => g.to_string(),
        })
        .collect();
    let shapes = Shapes::from_store_graphs(snap, &iris)?;
    let (classes, other_targets) = class_constraints(&shapes, Checked::OnRequest);
    Ok(ConstraintSource {
        kind: SourceKind::Graphs,
        graphs: graphs.to_vec(),
        file: false,
        mode: None,
        threshold: None,
        shapes: shapes.len(),
        other_targets,
        classes,
    })
}

fn mode_name(m: GuardMode) -> &'static str {
    match m {
        GuardMode::Reject => "reject",
        GuardMode::Warn => "warn",
        GuardMode::Off => "off",
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Violation => "violation",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// The source of the shapes an installed write-time guard validates every write against.
pub fn guard_source(guard: &ShaclGuard) -> ConstraintSource {
    configured_source(guard.config(), &guard.shapes())
}

/// The source of the shapes of a write-time validation configuration
/// ([`crate::guard::configured_shapes`]).
pub fn configured_source(cfg: &ValidationConfig, shapes: &Shapes) -> ConstraintSource {
    let checked = Checked::OnWrite {
        mode: cfg.mode,
        threshold: cfg.threshold,
    };
    let (classes, other_targets) = class_constraints(&shapes, checked);
    ConstraintSource {
        kind: SourceKind::Guard,
        graphs: cfg.shapes.graphs.clone().unwrap_or_default(),
        file: cfg.shapes.file.is_some() || cfg.shapes.inline.is_some(),
        mode: Some(mode_name(cfg.mode).to_string()),
        threshold: Some(severity_name(cfg.threshold).to_string()),
        shapes: shapes.len(),
        other_targets,
        classes,
    }
}
