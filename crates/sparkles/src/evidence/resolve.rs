//! Exact retained-reference and record resolution over a catalog (spec C25 §3 and §8).
//!
//! A [`Resolution`] verifies the references of one operation under one 64 MiB
//! preparation allowance, the request deadline and its cancellation. A stored-fact
//! reference names its dataset and branch by identity: the dataset is found among the
//! catalog's registered datasets by UUID, the branch by its UUID among the dataset's
//! listed branches, and the caller's access is decided on their current names before
//! anything is read. A missing dataset, branch or commit and an unreadable one give the
//! same `evidence-unavailable` answer.
//!
//! A verified result is data. It authorizes no write and is not a managed-write
//! capability.

pub use sparkles_core::evidence::resolve::{
    MAX_REFERENCES, MEMORY_BYTES, Receipt, RecordRequest, VerifiedFact, VerifiedRecord,
    VerifiedSpan, kind_iri, unavailable,
};
pub use sparkles_core::evidence::{BlankAnchor, GraphIdentity, RecordKind, StoredFactRef};

use crate::access::GraphAccess;
use crate::guard::WriteOptions;
use crate::task::Control;
use crate::{Catalog, Dataset, Result};
use sparkles_core::evidence::resolve::Resolver;
use std::sync::Arc;

/// One reference of a resolution request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    StoredFact(StoredFactRef),
    Record(RecordRequest),
}

/// The verified result of one reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verified {
    Fact(VerifiedFact),
    Record(VerifiedRecord),
}

/// The resolution state of one operation. Views of one commit are prepared once and
/// stay charged to the allowance until the resolution is dropped.
pub struct Resolution {
    resolver: Resolver,
    control: Control,
}

impl Resolution {
    /// A resolution with the profile's 64 MiB allowance under `control`.
    pub fn new(control: Control) -> Result<Self> {
        Self::with_memory(control, MEMORY_BYTES)
    }

    /// A resolution with `memory_bytes` of allowance, for embeddings that bound it
    /// further. The cap covers view preparation; reads of the result are not charged.
    pub fn with_memory(control: Control, memory_bytes: u64) -> Result<Self> {
        Ok(Self {
            resolver: Resolver::new(control.clone(), memory_bytes)?,
            control,
        })
    }

    fn options(&self) -> WriteOptions {
        WriteOptions {
            cancel: Some(self.control.cancel.flag()),
            deadline: self.control.deadline,
            ..Default::default()
        }
    }

    /// Verify a stored-fact reference among the datasets of `catalog`. `access` is
    /// asked what the caller may read on the referenced dataset and the current name of
    /// the referenced branch (`main` for the dataset itself), and returns `None` when it
    /// may read nothing there. It is not asked when the dataset or branch does not
    /// exist, which answers as unavailable in the same way.
    pub fn stored_fact(
        &mut self,
        key: &str,
        catalog: &Catalog,
        fact: &StoredFactRef,
        access: impl FnOnce(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    ) -> Result<VerifiedFact> {
        let Some(dataset) = catalog.try_get_by_id(fact.dataset, &self.options())? else {
            return Err(unavailable(key));
        };
        self.stored_fact_in(key, &dataset, fact, access)
    }

    /// [`stored_fact`](Self::stored_fact) for a reference into `dataset`, the main
    /// dataset an embedding already holds. A reference naming another dataset is
    /// unavailable.
    pub fn stored_fact_in(
        &mut self,
        key: &str,
        dataset: &Dataset,
        fact: &StoredFactRef,
        access: impl FnOnce(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    ) -> Result<VerifiedFact> {
        let main = dataset.store();
        if main.is_branch() || dataset.dataset_id() != fact.dataset {
            return Err(unavailable(key));
        }
        let granted = match main.listed_branch_name(fact.branch)? {
            Some(branch) => access(dataset, &branch),
            None => None,
        };
        self.resolver.stored_fact(key, main, granted.as_ref(), fact)
    }

    /// Verify a record reference at the current state of `logical`, which is `main` or
    /// one of its branches, under `access`, what the caller may read there (`None`:
    /// nothing). The record must be an authenticated profile record of the evidence
    /// settings' protected graphs, as [`Resolver`] describes.
    pub fn record(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        request: &RecordRequest,
    ) -> Result<VerifiedRecord> {
        self.resolver
            .record(key, main.store(), logical.store(), access.as_ref(), request)
    }

    /// Verify each reference in order, stopping at the first that does not verify.
    /// Stored facts are found in `catalog`, and records at the current state of
    /// `logical`. `access` is asked about a dataset and branch name, as
    /// [`stored_fact`](Self::stored_fact) describes, including `main`'s and `logical`'s
    /// for records.
    pub fn all(
        &mut self,
        catalog: Option<&Catalog>,
        main: &Dataset,
        logical: &Dataset,
        references: &[(String, Reference)],
        access: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    ) -> Result<Vec<Verified>> {
        if references.len() > MAX_REFERENCES {
            return Err(crate::Error::invalid(format!(
                "a resolution names at most {MAX_REFERENCES} references"
            )));
        }
        let logical_branch = if logical.store().is_branch() {
            main.store()
                .listed_branch_name(logical.store().branch_id())?
        } else {
            Some(crate::branch::MAIN.to_string())
        };
        let mut record_access: Option<Option<Arc<GraphAccess>>> = None;
        let mut out = Vec::with_capacity(references.len());
        for (key, reference) in references {
            self.control.check()?;
            out.push(match reference {
                Reference::StoredFact(fact) => Verified::Fact(match catalog {
                    Some(catalog) if fact.dataset != main.dataset_id() => {
                        self.stored_fact(key, catalog, fact, &mut *access)?
                    }
                    _ => self.stored_fact_in(key, main, fact, &mut *access)?,
                }),
                Reference::Record(request) => {
                    let granted = record_access
                        .get_or_insert_with(|| {
                            logical_branch
                                .as_deref()
                                .and_then(|branch| access(main, branch))
                        })
                        .clone();
                    Verified::Record(self.record(key, main, logical, granted, request)?)
                }
            });
        }
        Ok(out)
    }
}
