//! Shared error, option and lossy-axiom reporting types for OWL ontology syntax writers.

use crate::model::Axiom;
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
}

impl WriteReport {
    /// Record an axiom that could not be represented cleanly.
    pub fn record_skipped(&mut self, axiom: &Axiom, reason: impl Into<String>) {
        self.skipped_count += 1;
        if self.skipped.len() < MAX_REPORTED_SKIPPED {
            self.skipped.push(SkippedAxiom {
                axiom: axiom.clone(),
                reason: reason.into(),
            });
        }
    }

    /// Whether every axiom was written without loss.
    pub fn is_lossless(&self) -> bool {
        self.skipped_count == 0
    }

    pub(crate) fn skip_or_err(
        &mut self,
        options: WriteOptions,
        renderer: &crate::ofn::Renderer,
        ax: &Axiom,
        reason: &str,
    ) -> Result<(), WriteError> {
        if options.strict {
            return Err(WriteError::Unsupported(format!(
                "{reason}: {}",
                renderer.axiom(ax)
            )));
        }
        self.record_skipped(ax, reason);
        Ok(())
    }
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
