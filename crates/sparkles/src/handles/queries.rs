//! Stored queries: named, versioned SPARQL queries with typed parameters
//! (`queries.json`).

use crate::Dataset;
use crate::error::{Error, Result};
use crate::sparql::{QueryOptions, QueryResult};
use crate::stored::{Bindings, Change, Definition, Saved, Stored, Version};
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

    /// Every stored query visible to `viewer`, at its latest version.
    pub fn list_visible(
        &self,
        viewer: Option<&str>,
        is_admin: bool,
    ) -> Vec<(String, Stored)> {
        self.ds.state().queries.list_visible(viewer, is_admin)
    }

    /// Query `name` at `version`, or its latest version.
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored> {
        self.ds.state().queries.get(name, version)
    }

    /// Query `name` at `version` if visible to `viewer`, or its latest version.
    pub fn get_visible(
        &self,
        name: &str,
        version: Option<u64>,
        viewer: Option<&str>,
        is_admin: bool,
    ) -> Option<Stored> {
        self.ds
            .state()
            .queries
            .get_visible(name, version, viewer, is_admin)
    }

    /// The versions of query `name`, oldest first.
    pub fn versions(&self, name: &str) -> Option<Vec<Version>> {
        self.ds.state().queries.versions(name)
    }

    /// Create or replace query `name` as a new version.
    pub fn put(
        &self,
        name: &str,
        def: Definition,
        change: Change,
        viewer: Option<&str>,
        is_admin: bool,
    ) -> Result<Saved> {
        self.ds
            .state()
            .queries
            .put(name, def, change, viewer, is_admin)
    }

    /// Why `queries.json` could not be read, if it could not. The catalog is then empty
    /// and refuses changes, so the file is never overwritten.
    pub fn error(&self) -> Option<String> {
        self.ds.state().queries.broken().map(str::to_string)
    }

    /// Remove query `name` (if its latest version is `if_version`, when given); whether
    /// it existed.
    pub fn delete(
        &self,
        name: &str,
        if_version: Option<u64>,
        viewer: Option<&str>,
        is_admin: bool,
    ) -> Result<bool> {
        self.ds
            .state()
            .queries
            .delete(name, if_version, viewer, is_admin)
    }

    /// Run query `name` at its latest version with parameter values `params`: each is
    /// checked against its type, defaults fill the ones not given, and the query runs
    /// with `opts` and the values as initial bindings. The dataset's query defaults fill
    /// the fields `opts` leaves unset, as in [`Dataset::query_with`]. A missing query is
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
        let (stored, bindings) = self.bind(name, version, params)?;
        let mut opts = opts.clone();
        opts.initial_bindings.extend(bindings.initial);
        opts.list_bindings.extend(bindings.lists);
        self.ds.query_with(&stored.definition.query, &opts)
    }

    /// Query `name` at `version` (or its latest version) and the bindings of parameter
    /// values `params`, checked and completed as [`run`](Self::run) does, for a caller
    /// that runs the query its own way. Prefixed names in the values use the standard
    /// prefixes and the dataset's.
    pub fn bind(
        &self,
        name: &str,
        version: Option<u64>,
        params: &BTreeMap<String, serde_json::Value>,
    ) -> Result<(Stored, Bindings)> {
        let stored = self
            .get(name, version)
            .ok_or_else(|| Error::NotFound(format!("no stored query '{name}'")))?;
        let mut prefixes = crate::io::standard_prefixes();
        prefixes.extend(self.ds.store().prefixes());
        let bindings = stored.definition.bind(params, &prefixes)?;
        Ok((stored, bindings))
    }
}
