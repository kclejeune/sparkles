//! The dataset's GraphQL configuration (`graphql.json`, feature `graphql`).

use crate::Dataset;
use crate::error::Result;
use sparkles_graphql::{Stored, Version};

/// The dataset's GraphQL configuration (from [`Dataset::graphql`]).
#[derive(Clone)]
pub struct GraphQl {
    pub(crate) ds: Dataset,
}

impl GraphQl {
    /// The configuration at `version`, or the current one; `None` when there is none.
    pub fn get(&self, version: Option<u64>) -> Option<Stored> {
        self.ds.state().graphql.get(version)
    }

    /// The configuration's versions, oldest first.
    pub fn versions(&self) -> Vec<Version> {
        self.ds.state().graphql.versions()
    }

    /// Remove the configuration and its versions (if its current version is
    /// `if_version`, when given); whether there was one.
    pub fn reset(&self, if_version: Option<u64>) -> Result<bool> {
        self.ds.state().graphql.delete(if_version)
    }
}
