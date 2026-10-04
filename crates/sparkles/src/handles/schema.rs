//! Schema discovery over the dataset's current state.

use crate::Dataset;
use crate::error::{Error, Result};
use crate::schema::SchemaError;
use crate::schema::draft::{DraftOptions, ShapesDraft};
use crate::schema::profile::{ClassProfiles, ProfileOptions};

/// Schema discovery (from [`Dataset::schema`]).
#[derive(Clone)]
pub struct Schema {
    pub(crate) ds: Dataset,
}

impl Schema {
    /// For each class, the predicates its instances use and the kinds of their values.
    pub fn profiles(&self, opts: &ProfileOptions) -> Result<ClassProfiles> {
        crate::schema::profile::profiles(&self.ds.snapshot(), opts).map_err(schema_error)
    }

    /// SHACL shapes drafted from the data.
    pub fn draft_shapes(&self, opts: &DraftOptions) -> Result<ShapesDraft> {
        crate::schema::draft::draft_shapes(&self.ds.snapshot(), opts).map_err(schema_error)
    }
}

/// The engine error of a schema discovery error: the deadline and the cancel flag keep
/// their meaning, a missing graph is `NotFound`, and a selection over its limit is
/// `Invalid`.
pub(crate) fn schema_error(e: SchemaError) -> Error {
    match e {
        SchemaError::Store(e) => e,
        SchemaError::Timeout { .. } => Error::Timeout,
        SchemaError::Cancelled => Error::Cancelled,
        SchemaError::NoSuchGraph(_) => Error::NotFound(e.to_string()),
        #[allow(unreachable_patterns)]
        e => Error::invalid(e.to_string()),
    }
}
