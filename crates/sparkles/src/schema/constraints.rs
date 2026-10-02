//! The constraints layer of a schema report: the SHACL shapes a dataset uses, listed per
//! target class.
//!
//! The layer is kept apart from the observed and declared layers. It states what the
//! shapes require, and each property constraint says what checks it: write-time
//! validation that rejects a write that breaks it, write-time validation that only
//! reports it, or nothing until someone validates the data. Nothing in this layer is
//! derived from the data, and nothing in the observed layer is derived from it.
//!
//! This module holds the types only. `sparkles_shacl::constraints` builds them from
//! parsed shapes.

use serde::Serialize;

/// What checks a constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforcement {
    /// Write-time validation in `reject` mode refuses a write that breaks it.
    RejectOnWrite,
    /// Write-time validation commits a write that breaks it and reports the results.
    WarnOnWrite,
    /// Nothing checks it until the data is validated on request, for example with
    /// `POST /{ds}/shacl`.
    ValidatedOnRequest,
}

impl Enforcement {
    /// `reject-on-write`, `warn-on-write` or `validated-on-request`.
    pub fn name(self) -> &'static str {
        match self {
            Enforcement::RejectOnWrite => "reject-on-write",
            Enforcement::WarnOnWrite => "warn-on-write",
            Enforcement::ValidatedOnRequest => "validated-on-request",
        }
    }
}

/// Where a source's shapes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// The shapes of the dataset's write-time validation.
    Guard,
    /// Shapes graphs of the dataset that the request names.
    Graphs,
}

/// The constraints layer: one entry per source of shapes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ConstraintsLayer {
    pub sources: Vec<ConstraintSource>,
}

impl ConstraintsLayer {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// The shapes of one source, listed per target class.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintSource {
    pub kind: SourceKind,
    /// The shapes graphs read: graph IRIs, or `default` for the default graph. For the
    /// guard these are the shapes graphs its configuration designates.
    pub graphs: Vec<String>,
    /// The guard also has shapes given as a file or inline.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub file: bool,
    /// The guard's mode, `reject` or `warn`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The guard's threshold: in `reject` mode, results of this severity or above block
    /// a write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<String>,
    /// All shapes of the source, node and property shapes.
    pub shapes: usize,
    /// Active shapes whose targets are not classes (`sh:targetNode`,
    /// `sh:targetSubjectsOf`, `sh:targetObjectsOf`, `sh:targetWhere`). The layer does not
    /// list them.
    pub other_targets: usize,
    /// Sorted by class IRI.
    pub classes: Vec<ClassConstraints>,
}

/// The constraints on the instances of one class.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassConstraints {
    pub class: String,
    /// The IRIs of the shapes that target the class with `sh:targetClass` or an implicit
    /// class target. Shapes that are blank nodes are not named.
    pub shapes: Vec<String>,
    /// A shape of the class has `sh:closed true`.
    pub closed: bool,
    /// The property shapes with a predicate path, sorted by path.
    pub properties: Vec<PropertyConstraint>,
    /// Property shapes of the class whose path is not a single predicate. They are not
    /// listed.
    pub other_paths: usize,
}

/// One property shape with a predicate path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyConstraint {
    /// The predicate IRI.
    pub path: String,
    /// The property shape's IRI, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    /// `sh:severity` of the property shape (an IRI, `sh:Violation` by default).
    pub severity: String,
    pub enforcement: Enforcement,
    /// `sh:minCount` (the largest, if several).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_count: Option<u64>,
    /// `sh:maxCount` (the smallest, if several).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u64>,
    /// `sh:datatype`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    /// `sh:class`, each class the values must be instances of.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub class: Vec<String>,
    /// `sh:nodeKind`, as the IRI of the node kind, such as `sh:IRI`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_kind: Option<String>,
    /// The constraint components of the shape's other constraints, such as
    /// `sh:PatternConstraintComponent`, sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other: Vec<String>,
}

impl PropertyConstraint {
    /// A short line such as `min 1 · max 1 · datatype xsd:string · class ex:Org`, with
    /// IRIs shortened by `short`.
    pub fn summary(&self, short: impl Fn(&str) -> String) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(n) = self.min_count {
            parts.push(format!("min {n}"));
        }
        if let Some(n) = self.max_count {
            parts.push(format!("max {n}"));
        }
        if let Some(d) = &self.datatype {
            parts.push(format!("datatype {}", short(d)));
        }
        for c in &self.class {
            parts.push(format!("class {}", short(c)));
        }
        if let Some(k) = &self.node_kind {
            parts.push(format!("nodeKind {}", short(k)));
        }
        for o in &self.other {
            let name = o
                .rsplit(['#', '/'])
                .next()
                .unwrap_or(o)
                .trim_end_matches("ConstraintComponent");
            parts.push(format!("+{name}"));
        }
        if parts.is_empty() {
            "no constraints".to_string()
        } else {
            parts.join(" · ")
        }
    }
}
