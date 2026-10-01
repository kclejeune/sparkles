//! Imports and EXTERNAL shapes: where their definitions come from ([`Resolver`]), and
//! the closure of a schema over them ([`close`]).

use crate::ast::{Schema, ShapeExpr};
use crate::error::{SchemaError, schema_todo, todo};
use sparkles::outbound::{OutboundPolicy, RequestBudget};
use sparkles::sparql::FileLoads;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Supplies imported schemas and the definitions of EXTERNAL shapes.
pub trait Resolver: Send + Sync {
    /// The schema behind an `IMPORT` IRI; `None` if this resolver does not know it.
    fn import(&self, iri: &str) -> anyhow::Result<Option<Schema>>;

    /// The definition of an EXTERNAL shape, by its label's ShExJ form (the IRI, or
    /// `_:label`); `None` if this resolver does not know it.
    fn external(&self, label: &str) -> anyhow::Result<Option<ShapeExpr>> {
        let _ = label;
        Ok(None)
    }
}

/// A resolver that knows no imports and no external shapes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoImports;

impl Resolver for NoImports {
    fn import(&self, _iri: &str) -> anyhow::Result<Option<Schema>> {
        Ok(None)
    }
}

/// Imports from inline bodies, then files, then http(s); external shapes from an externs
/// schema. An IRI that does not resolve as given is tried with `.shex`, then `.json`
/// appended.
#[derive(Default)]
pub struct FileResolver {
    /// directories relative IRIs resolve against (the importing schema's, on the
    /// command line)
    pub dirs: Vec<PathBuf>,
    /// import bodies given with the request, by IRI
    pub inline: HashMap<String, Schema>,
    /// which `file:` IRIs may be read
    pub files: FileLoads,
    /// http(s) imports, through this policy and request budget (`None`: no network)
    pub outbound: Option<(OutboundPolicy, Arc<RequestBudget>)>,
    /// the schema whose shapes define the EXTERNAL labels (`--externs`, the envelope's
    /// `externs`)
    pub externs: Option<Schema>,
}

impl Resolver for FileResolver {
    fn import(&self, _iri: &str) -> anyhow::Result<Option<Schema>> {
        Err(todo("imports"))
    }

    fn external(&self, _label: &str) -> anyhow::Result<Option<ShapeExpr>> {
        Err(todo("external shapes"))
    }
}

/// The schema with its import closure merged in (each IRI fetched once, cycles ended,
/// imported `start`s ignored) and EXTERNAL shapes replaced by their definitions where
/// `resolver` has one. Overlapping labels and imported start actions are errors.
pub fn close(schema: &Schema, resolver: &dyn Resolver) -> Result<Schema, SchemaError> {
    let _ = resolver;
    let external = schema
        .shapes
        .iter()
        .any(|d| matches!(d.expr, ShapeExpr::External));
    if schema.imports.is_empty() && !external {
        return Ok(schema.clone());
    }
    Err(schema_todo("imports and external shapes"))
}
