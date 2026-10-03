//! Shorthands for the engine's own tests. They do what the methods of the same names on
//! the `sparkles` crate's `Dataset` do, so the tests that used a `Dataset` before it moved
//! out of the engine run unchanged on a `Store`.

use super::Store;
use crate::error::Result;
use crate::io::{RdfFormat, Source};
use crate::sparql::update::UpdateStats;
use crate::sparql::{QueryOptions, QueryResult};

impl Store {
    /// Load RDF text in one commit and return the number of new quads.
    pub(crate) fn load_str(&self, data: &str, format: RdfFormat) -> Result<u64> {
        self.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
    }

    /// Run a SPARQL query with the store's DESCRIBE setting.
    pub(crate) fn query(&self, query: &str) -> Result<QueryResult> {
        let opts = QueryOptions {
            describe: self.describe_settings(),
            ..Default::default()
        };
        crate::sparql::query(self.snapshot(), query, &opts)
    }

    /// Run a SPARQL Update request with the default options.
    pub(crate) fn update(&self, update: &str) -> Result<UpdateStats> {
        crate::sparql::update::update(self, update, &QueryOptions::default())
    }
}
