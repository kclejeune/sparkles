//! Resident source ownership for trusted adapters. Resolving a source does not
//! authenticate a caller or grant RDF, Jobs or Evidence access.

use crate::access::source::{SourceOwner, Target};
use crate::guard::WriteOptions;
use crate::store::Store;
use crate::{Catalog, Dataset, Result};
use std::sync::Arc;

/// Retains the catalog's real DatasetState, including its Own(Store) payload.
/// A logical branch also retains its existing resident Store Arc. No branch
/// Dataset or runtime is constructed as a side effect of source resolution.
pub struct ResidentSourceOwner {
    main: Dataset,
    branch: Option<Arc<Store>>,
}

impl ResidentSourceOwner {
    /// Resolve only an already registered main and an already resident branch.
    /// Missing, removed and nonresident identities all return None. Authorization
    /// and current routing-generation checks belong to the trusted host adapter.
    pub fn resolve(
        catalog: &Catalog,
        target: Target,
        options: &WriteOptions,
    ) -> Result<Option<Self>> {
        let Some(main) = catalog.try_get_by_id(target.dataset, options)? else {
            return Ok(None);
        };
        let branch = if target.branch == target.dataset {
            None
        } else {
            let Some(branch) = main.store().try_resident_branch(target.branch, options)? else {
                return Ok(None);
            };
            Some(branch)
        };
        let owner = Self { main, branch };
        options.check()?;
        Ok(Some(owner))
    }
}

impl SourceOwner for ResidentSourceOwner {
    fn actual_main(&self) -> &Store {
        self.main.store()
    }

    fn logical(&self) -> &Store {
        self.branch.as_deref().unwrap_or_else(|| self.main.store())
    }
}
