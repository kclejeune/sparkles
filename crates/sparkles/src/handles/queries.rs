//! Stored queries: named, versioned SPARQL queries with typed parameters
//! (`queries.json`).

use crate::Dataset;
use crate::error::{Error, Result};
use crate::sparql::{QueryOptions, QueryResult};
use crate::stored::{Change, Definition, Saved, Stored, Version};
use std::collections::BTreeMap;

/// The dataset's stored queries (from [`Dataset::queries`]).
#[derive(Clone)]
pub struct StoredQueries {
    pub(crate) ds: Dataset,
}

impl StoredQueries {
    /// Every stored query at its latest version, by name.
    pub fn list(&self) -> Vec<(String, Stored)> {
        self.ds.state().queries.list()
    }

    /// Query `name` at `version`, or its latest version.
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored> {
        self.ds.state().queries.get(name, version)
    }

    /// The versions of query `name`, oldest first.
    pub fn versions(&self, name: &str) -> Option<Vec<Version>> {
        self.ds.state().queries.versions(name)
    }

    /// Create or replace query `name` as a new version.
    pub fn put(&self, name: &str, def: Definition, change: Change) -> Result<Saved> {
        self.ds.state().queries.put(name, def, change)
    }

    /// Why `queries.json` could not be read, if it could not. The catalog is then empty
    /// and refuses changes, so the file is never overwritten.
    pub fn error(&self) -> Option<String> {
        self.ds.state().queries.broken().map(str::to_string)
    }

    /// Remove query `name` (if its latest version is `if_version`, when given); whether
    /// it existed.
    pub fn delete(&self, name: &str, if_version: Option<u64>) -> Result<bool> {
        self.ds.state().queries.delete(name, if_version)
    }

    /// Run query `name` at its latest version with parameter values `params`: each is
    /// checked against its type, defaults fill the ones not given, and the query runs
    /// with `opts` and the values as initial bindings. A missing query is
    /// [`Error::NotFound`]; a value that does not fit is [`Error::Invalid`].
    pub fn run(
        &self,
        name: &str,
        params: &BTreeMap<String, serde_json::Value>,
        opts: &QueryOptions,
    ) -> Result<QueryResult> {
        self.run_version(name, None, params, opts)
    }

    /// [`run`](Self::run) at `version` of query `name` while it is kept, or at its
    /// latest version. A missing version is [`Error::NotFound`].
    pub fn run_version(
        &self,
        name: &str,
        version: Option<u64>,
        params: &BTreeMap<String, serde_json::Value>,
        opts: &QueryOptions,
    ) -> Result<QueryResult> {
        let stored = self
            .get(name, version)
            .ok_or_else(|| Error::NotFound(format!("no stored query '{name}'")))?;
        let mut prefixes = crate::io::standard_prefixes();
        prefixes.extend(self.ds.store().prefixes());
        let bindings = stored.definition.bind(params, &prefixes)?;
        let mut opts = opts.clone();
        opts.initial_bindings.extend(bindings);
        self.ds.query_with(&stored.definition.query, &opts)
    }
}
