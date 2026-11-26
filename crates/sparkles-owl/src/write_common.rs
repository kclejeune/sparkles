//! Shared error, option and lossy-axiom reporting types for OWL ontology syntax writers.

use crate::model::{Annotation, Axiom, AxiomKind, ClassExpression, DataRange, Header};
use crate::read::MAX_NESTING;
use std::fmt;

/// The maximum number of skipped-axiom entries retained in a [`WriteReport`], keeping
/// memory bounded even on large ontologies written to a lossy target syntax.
pub const MAX_REPORTED_SKIPPED: usize = 1_000;

/// Options shared by ontology syntax writers that may encounter axioms a target syntax
/// cannot represent cleanly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// When `true`, refuse the first axiom or construct the target syntax cannot
    /// represent cleanly instead of skipping it and recording it in [`WriteReport`].
    pub strict: bool,
}

/// An axiom that a target syntax could not represent cleanly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedAxiom {
    pub axiom: Axiom,
    pub reason: String,
}

/// Outcome of writing an ontology document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteReport {
    /// Number of axioms written (or translated into the target syntax).
    pub written_axioms: usize,
    /// Total number of axioms skipped because the target syntax cannot represent them.
    pub skipped_count: usize,
    /// Bounded sample (up to [`MAX_REPORTED_SKIPPED`]) of skipped axioms and reasons.
    pub skipped: Vec<SkippedAxiom>,
    /// Total number of ontology header annotations skipped because the target syntax
    /// cannot represent them.
    pub skipped_header_count: usize,
    /// Bounded sample (up to [`MAX_REPORTED_SKIPPED`]) of skipped header annotation
    /// diagnostics.
    pub skipped_header: Vec<String>,
}

impl WriteReport {
    /// Record an axiom that could not be represented cleanly.
    pub fn record_skipped(&mut self, axiom: &Axiom, reason: impl Into<String>) {
        self.skipped_count += 1;
        if self.skipped.len() < MAX_REPORTED_SKIPPED && !axiom_exceeds_nesting(axiom) {
            self.skipped.push(SkippedAxiom {
                axiom: axiom.clone(),
                reason: reason.into(),
            });
        }
    }

    /// Record an ontology header annotation that could not be represented cleanly.
    pub fn record_skipped_header(&mut self, reason: impl Into<String>) {
        self.skipped_header_count += 1;
        if self.skipped_header.len() < MAX_REPORTED_SKIPPED {
            self.skipped_header.push(reason.into());
        }
    }

    /// Whether every axiom and header annotation was written without loss.
    pub fn is_lossless(&self) -> bool {
        self.skipped_count == 0 && self.skipped_header_count == 0
    }

    pub(crate) fn skip_or_err(
        &mut self,
        options: WriteOptions,
        renderer: &crate::ofn::Renderer,
        ax: &Axiom,
        reason: &str,
    ) -> Result<(), WriteError> {
        if options.strict {
            let rendered = if axiom_exceeds_nesting(ax) {
                format!(
                    "{} (exceeds max nesting depth {MAX_NESTING})",
                    ax.axiom_type().name()
                )
            } else {
                renderer.axiom(ax)
            };
            return Err(WriteError::Unsupported(format!("{reason}: {rendered}")));
        }
        self.record_skipped(ax, reason);
        Ok(())
    }

    pub(crate) fn skip_header_or_err(
        &mut self,
        options: WriteOptions,
        renderer: &crate::ofn::Renderer,
        ann: &Annotation,
        reason: &str,
    ) -> Result<(), WriteError> {
        let rendered = if max_ann_depth(ann, 1) > MAX_NESTING {
            format!(
                "Annotation({} … exceeds max nesting depth {MAX_NESTING})",
                ann.property.as_str()
            )
        } else {
            renderer.annotation(ann)
        };
        if options.strict {
            return Err(WriteError::Unsupported(format!("{reason}: {rendered}")));
        }
        self.record_skipped_header(format!("{reason}: {rendered}"));
        Ok(())
    }
}

pub(crate) fn max_ce_depth(c: &ClassExpression, d: usize) -> usize {
    use ClassExpression as C;
    if d > MAX_NESTING {
        return d;
    }
    match c {
        C::Class(_)
        | C::ObjectOneOf(_)
        | C::ObjectHasValue(..)
        | C::ObjectHasSelf(_)
        | C::DataHasValue(..) => d,
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().map(|x| max_ce_depth(x, d + 1)).max().unwrap_or(d)
        }
        C::ObjectComplementOf(x) | C::ObjectSomeValuesFrom(_, x) | C::ObjectAllValuesFrom(_, x) => {
            max_ce_depth(x, d + 1)
        }
        C::ObjectMinCardinality(_, _, f)
        | C::ObjectMaxCardinality(_, _, f)
        | C::ObjectExactCardinality(_, _, f) => f.as_deref().map_or(d, |x| max_ce_depth(x, d + 1)),
        C::DataSomeValuesFrom(_, r) | C::DataAllValuesFrom(_, r) => max_dr_depth(r, d + 1),
        C::DataMinCardinality(_, _, r)
        | C::DataMaxCardinality(_, _, r)
        | C::DataExactCardinality(_, _, r) => r.as_ref().map_or(d, |x| max_dr_depth(x, d + 1)),
    }
}

pub(crate) fn max_dr_depth(r: &DataRange, d: usize) -> usize {
    if d > MAX_NESTING {
        return d;
    }
    match r {
        DataRange::Datatype(_) | DataRange::OneOf(_) | DataRange::DatatypeRestriction(..) => d,
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
            rs.iter().map(|x| max_dr_depth(x, d + 1)).max().unwrap_or(d)
        }
        DataRange::ComplementOf(x) => max_dr_depth(x, d + 1),
    }
}

pub(crate) fn max_ann_depth(a: &Annotation, d: usize) -> usize {
    if d > MAX_NESTING {
        return d;
    }
    a.annotations
        .iter()
        .map(|n| max_ann_depth(n, d + 1))
        .max()
        .unwrap_or(d)
}

pub(crate) fn axiom_exceeds_nesting(ax: &Axiom) -> bool {
    if ax
        .annotations
        .iter()
        .any(|a| max_ann_depth(a, 1) > MAX_NESTING)
    {
        return true;
    }
    use AxiomKind as K;
    let d = match &ax.kind {
        K::SubClassOf(a, b) => max_ce_depth(a, 1).max(max_ce_depth(b, 1)),
        K::EquivalentClasses(cs) => cs.iter().map(|c| max_ce_depth(c, 1)).max().unwrap_or(1),
        K::DisjointClasses(cs) => cs.iter().map(|c| max_ce_depth(c, 1)).max().unwrap_or(1),
        K::DisjointUnion(_, cs) => cs.iter().map(|c| max_ce_depth(c, 1)).max().unwrap_or(1),
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::HasKey(c, _, _)
        | K::ClassAssertion(c, _) => max_ce_depth(c, 1),
        K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => max_dr_depth(r, 1),
        _ => 1,
    };
    d > MAX_NESTING
}

pub(crate) fn check_header_depth(header: &Header, syntax: &str) -> Result<(), WriteError> {
    for a in &header.annotations {
        if max_ann_depth(a, 1) > MAX_NESTING {
            return Err(WriteError::Unsupported(format!(
                "{syntax} header annotation nests deeper than {MAX_NESTING} levels"
            )));
        }
    }
    Ok(())
}

pub(crate) fn check_axiom_depth(ax: &Axiom, syntax: &str) -> Result<(), WriteError> {
    for a in &ax.annotations {
        if max_ann_depth(a, 1) > MAX_NESTING {
            return Err(WriteError::Unsupported(format!(
                "{syntax} annotation nests deeper than {MAX_NESTING} levels"
            )));
        }
    }
    if axiom_exceeds_nesting(ax) {
        return Err(WriteError::Unsupported(format!(
            "{syntax} expression nests deeper than {MAX_NESTING} levels"
        )));
    }
    Ok(())
}

/// An error from an ontology syntax writer.
#[derive(Debug)]
pub enum WriteError {
    /// An underlying I/O error while streaming output.
    Io(std::io::Error),
    /// A construct or character the target syntax cannot represent (or a strict refusal).
    Unsupported(String),
}

impl From<std::io::Error> for WriteError {
    fn from(e: std::io::Error) -> Self {
        WriteError::Io(e)
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Io(e) => write!(f, "{e}"),
            WriteError::Unsupported(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Io(e) => Some(e),
            WriteError::Unsupported(_) => None,
        }
    }
}
