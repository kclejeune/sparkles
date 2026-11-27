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
    GraphNodes, MAX_REFERENCES, MEMORY_BYTES, SingleObject, Phase2Outcome, ProposedPassage, ProposedStructured, Receipt,
    RecordRequest, SelectionProbe, SelectionRequirements, VerifiedFact, VerifiedRecord,
    VerifiedSpan, check_anchor, kind_iri, unavailable,
};
pub use sparkles_core::evidence::{BlankAnchor, GraphIdentity, RecordKind, StoredFactRef};

use crate::access::GraphAccess;
use crate::guard::WriteOptions;
use crate::task::Control;
use crate::{Catalog, Dataset, Result};
use oxrdf::NamedNode;
use sparkles_core::evidence::resolve::Resolver;
use std::sync::Arc;

/// Whether `code` is one of the resolution outcomes, whose answers name the reference
/// that did not verify.
pub fn is_outcome(code: &str) -> bool {
    matches!(
        code,
        "evidence-unavailable"
            | "incomplete-evidence"
            | "unanchored-evidence"
            | "span-mismatch"
            | "origin-not-reproduced"
            | "immutable-record-conflict"
    )
}

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
        // The anchor is decided before the lookup, so its answer does not depend on
        // whether the dataset exists.
        check_anchor(key, fact)?;
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
        check_anchor(key, fact)?;
        let main = dataset.store();
        if main.is_branch() || dataset.dataset_id() != fact.dataset {
            return Err(unavailable(key));
        }
        let granted = match main.listed_branch_name(fact.branch, &self.options())? {
            Some(branch) => access(dataset, &branch),
            None => None,
        };
        self.resolver.stored_fact(key, main, granted.as_ref(), fact)
    }

    /// The operation's shared preparation context.
    pub fn preparation(&self) -> &crate::access::Preparation {
        self.resolver.preparation()
    }

    /// Pin record and graph lookups on `(main, logical)` to the exact branch commit
    /// `commit`, so every call in this resolution reads one consistent snapshot.
    pub fn pin_commit(&mut self, main: &Dataset, logical: &Dataset, commit: u64) {
        self.resolver
            .pin_commit(main.store(), logical.store(), commit);
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

    /// Probe a record reference and its selection-target availability without
    /// reconstructing a compatible retained literal (`ev:normalizedText` or
    /// `ev:retainedBytes`) when a single `Vocab`/`Delta` object ID is present.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn probe_record_with_selection_at(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
        selection: SelectionRequirements,
        req_idx: usize,
    ) -> Result<SelectionProbe> {
        self.resolver.probe_record_with_selection_at(
            key,
            main.store(),
            logical.store(),
            access.as_ref(),
            iri,
            kinds,
            selection,
            req_idx,
        )
    }

    /// Smallest Phase 1 requirement index among deferred existing passages that have not
    /// yet been verified or consumed.
    pub(crate) fn next_pending_passage_req_idx(&self) -> Option<usize> {
        self.resolver.next_pending_passage_req_idx()
    }

    /// Verify and consume the earliest deferred existing passage.
    pub(crate) fn verify_next_pending_passage(
        &mut self,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
    ) -> Phase2Outcome {
        self.resolver
            .verify_next_pending_passage(main.store(), logical.store(), access.as_ref())
    }

    /// Verify the exact vocabulary literal shape of a selected target whose single
    /// `Vocab`/`Delta` object ID was deferred by `probe_record_with_selection_at`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn verify_selection_literal_shape(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        iri: &NamedNode,
        selection: SelectionRequirements,
    ) -> Result<()> {
        self.resolver.verify_selection_literal_shape(
            key,
            main.store(),
            logical.store(),
            access.as_ref(),
            iri,
            selection,
        )
    }

    /// Resolve the caller's read access for `logical` (`main` or one of its listed
    /// branches) by its current branch name.
    pub(crate) fn logical_access(
        &self,
        main: &Dataset,
        logical: &Dataset,
        access: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    ) -> Result<Option<Arc<GraphAccess>>> {
        let branch = if logical.store().is_branch() {
            main.store()
                .listed_branch_name(logical.store().branch_id(), &self.options())?
        } else {
            Some(crate::branch::MAIN.to_string())
        };
        Ok(branch.as_deref().and_then(|name| access(main, name)))
    }

    /// Verify one or more proposed passages against an existing authenticated rendition
    /// at the current state of `logical`.
    pub fn verify_proposed_passages(
        &mut self,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        rendition: &NamedNode,
        passages: &[ProposedPassage<'_>],
    ) -> Result<()> {
        self.resolver.verify_proposed_passages(
            main.store(),
            logical.store(),
            access.as_ref(),
            rendition,
            passages,
        )
    }

    /// Verify proposed passages and any deferred existing passages targeting `rendition`
    /// in one `ev:normalizedText` scan, returning Phase 2 fallback context.
    pub(crate) fn verify_proposed_passages_checked(
        &mut self,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        rendition: &NamedNode,
        passages: &[ProposedPassage<'_>],
        group_req_idx: usize,
    ) -> Phase2Outcome {
        self.resolver.verify_proposed_passages_checked(
            main.store(),
            logical.store(),
            access.as_ref(),
            rendition,
            passages,
            group_req_idx,
        )
    }

    /// Verify that an existing authenticated passage belongs to `expected_revision` and
    /// `expected_rendition`, and that its own selector verifies against its rendition's
    /// retained text.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_passage_reference(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        passage: &NamedNode,
        expected_revision: &NamedNode,
        expected_rendition: &NamedNode,
    ) -> Result<()> {
        self.resolver.verify_passage_reference(
            key,
            main.store(),
            logical.store(),
            access.as_ref(),
            passage,
            expected_revision,
            expected_rendition,
        )
    }

    /// Verify one or more structured JSON Pointer references (`json-lexical-utf8:1`)
    /// against an existing authenticated source revision's retained bytes.
    pub fn verify_structured_references(
        &mut self,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        revision: &NamedNode,
        references: &[ProposedStructured<'_>],
    ) -> Result<()> {
        self.resolver.verify_structured_references(
            main.store(),
            logical.store(),
            access.as_ref(),
            revision,
            references,
        )
    }

    /// Verify structured JSON Pointer references against `revision`, returning Phase 2
    /// fallback context.
    pub(crate) fn verify_structured_references_checked(
        &mut self,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        revision: &NamedNode,
        references: &[ProposedStructured<'_>],
    ) -> Phase2Outcome {
        self.resolver.verify_structured_references_checked(
            main.store(),
            logical.store(),
            access.as_ref(),
            revision,
            references,
        )
    }

    /// Verify a record reference as [`Self::record`], borrowing `iri` and returning a
    /// [`crate::access::Reservation`] that keeps the variable-sized strings inside
    /// [`VerifiedRecord`] charged to this resolution's preparation allowance until dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn record_charged(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: Option<Arc<GraphAccess>>,
        iri: &oxrdf::NamedNode,
        kinds: &[RecordKind],
        expected_content_digest: Option<&sparkles_core::evidence::Sha256Digest>,
    ) -> Result<(VerifiedRecord, crate::access::Reservation)> {
        self.resolver.record_charged(
            key,
            main.store(),
            logical.store(),
            access.as_ref(),
            iri,
            kinds,
            expected_content_digest,
        )
    }

    /// Whether `s p o` is visible in `graph` at the resolving state of `logical` under
    /// `access`, bounded by this resolution's preparation allowance and controls.
    #[allow(clippy::too_many_arguments)]
    pub fn has_triple(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: &Arc<GraphAccess>,
        graph: &oxrdf::NamedNode,
        s: &oxrdf::NamedNode,
        p: &oxrdf::NamedNode,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        self.resolver
            .has_triple(key, main.store(), logical.store(), access, graph, s, p, o)
    }

    /// Whether at least one triple `s p ?o` is visible in `graph` at the resolving state of
    /// `logical` under `access`, without decoding any object term.
    #[allow(clippy::too_many_arguments)]
    pub fn has_any_object_in_graph(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: &Arc<GraphAccess>,
        graph: &oxrdf::NamedNode,
        s: &oxrdf::NamedNode,
        p: &oxrdf::NamedNode,
    ) -> Result<bool> {
        self.resolver.has_any_object_in_graph(
            key,
            main.store(),
            logical.store(),
            access,
            graph,
            s,
            p,
        )
    }

    /// Inspect the object(s) of `s p ?o` visible in `graph` at the resolving state of
    /// `logical` under `access`, borrowing the single decoded term inside `parse`.
    #[allow(clippy::too_many_arguments)]
    pub fn single_object_in_graph<T>(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: &Arc<GraphAccess>,
        graph: &oxrdf::NamedNode,
        s: &oxrdf::NamedNode,
        p: &oxrdf::NamedNode,
        parse: impl FnOnce(Option<&oxrdf::Term>) -> Result<T>,
    ) -> Result<SingleObject<T>> {
        self.resolver.single_object_in_graph(
            key,
            main.store(),
            logical.store(),
            access,
            graph,
            s,
            p,
            parse,
        )
    }

    /// Named-node objects of `s p ?o` visible in `graph`, collecting at most `limit`
    /// results and decrementing `remaining_scans` for each scanned quad in `graph`.
    #[allow(clippy::too_many_arguments)]
    pub fn objects_in_graph(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: &Arc<GraphAccess>,
        graph: &oxrdf::NamedNode,
        s: &oxrdf::NamedNode,
        p: &oxrdf::NamedNode,
        limit: usize,
        remaining_scans: &mut u64,
    ) -> Result<GraphNodes> {
        self.resolver.objects_in_graph(
            key,
            main.store(),
            logical.store(),
            access,
            graph,
            s,
            p,
            limit,
            remaining_scans,
        )
    }

    /// Named-node subjects of `?s p o` visible in `graph`, collecting at most `limit`
    /// results and decrementing `remaining_scans` only for quads belonging to `graph`.
    #[allow(clippy::too_many_arguments)]
    pub fn subjects_in_graph(
        &mut self,
        key: &str,
        main: &Dataset,
        logical: &Dataset,
        access: &Arc<GraphAccess>,
        graph: &oxrdf::NamedNode,
        p: &oxrdf::NamedNode,
        o: &oxrdf::Term,
        limit: usize,
        remaining_scans: &mut u64,
    ) -> Result<GraphNodes> {
        self.resolver.subjects_in_graph(
            key,
            main.store(),
            logical.store(),
            access,
            graph,
            p,
            o,
            limit,
            remaining_scans,
        )
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
                .listed_branch_name(logical.store().branch_id(), &self.options())?
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
