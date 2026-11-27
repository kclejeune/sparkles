//! Closed-directory imported-origin helpers for clones, new-identity restores,
//! read-only validation (`sparkles check`), WAL binding lookup, and backup capture.

use super::*;
use crate::access::Reservation;
use crate::commit::{ForkedFrom, RetainedGenerationCommit};
use crate::guard::WriteOptions;
use crate::managed::Installed;
use crate::store::cold::ColdBudget;
use crate::store::{CapturedFile, FileKind, FileSource};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

/// Verified on-disk imported closure loaded without a live `Family` lock (used by
/// read-only validation, WAL binding lookup, backup capture, and closed-directory
/// reidentification).
pub(in crate::store) struct DiskClosure {
    pub(super) origin_digest: Sha256Digest,
    pub(super) history_dir: documents::Opened,
    pub(super) origin: managed_history::Owned<Origin>,
    pub(super) nodes: Vec<DiskNode>,
    _held: Reservation,
}

pub(super) struct DiskNode {
    pub(super) digest: Sha256Digest,
    pub(super) descriptor: managed_history::Owned<Descriptor>,
    pub(super) directory: documents::Opened,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::store) struct ProveWalStage {
    pub(in crate::store) stage: &'static str,
    pub(in crate::store) budget_used: u64,
    pub(in crate::store) budget_available: u64,
    pub(in crate::store) sub_limit: Option<u64>,
    pub(in crate::store) sub_used: Option<u64>,
}

#[cfg(test)]
pub(in crate::store) type ProveWalStageHook = Option<Box<dyn FnMut(ProveWalStage)>>;

#[cfg(test)]
thread_local! {
    /// Test-only stage observer for `DiskClosure::prove_wal` that reports
    /// admission boundaries and sub-arena allowances.
    pub(in crate::store) static PROVE_WAL_STAGE_HOOK: std::cell::RefCell<ProveWalStageHook> =
        const { std::cell::RefCell::new(None) };
}

fn prove_wal_stage_hook(
    _stage: &'static str,
    _budget_used: u64,
    _budget_available: u64,
    _sub_limit: Option<u64>,
    _sub_used: Option<u64>,
) {
    #[cfg(test)]
    PROVE_WAL_STAGE_HOOK.with(|hook| {
        if let Some(cb) = hook.borrow_mut().as_mut() {
            cb(ProveWalStage {
                stage: _stage,
                budget_used: _budget_used,
                budget_available: _budget_available,
                sub_limit: _sub_limit,
                sub_used: _sub_used,
            });
        }
    });
}

impl DiskClosure {
    /// Load and authenticate `evidence-history/origin.json` and every declared
    /// imported source under `evidence-history/imports/`. Returns `Ok(None)` if
    /// `origin.json` is absent and no orphan `imports/` directory exists.
    pub(in crate::store) fn load(root: &Path, preparation: &Preparation) -> Result<Option<Self>> {
        Self::load_inner(root, None, preparation)
    }

    /// Like [`Self::load`], additionally checking `origin.json` against the live
    /// `Family`'s pinned opening origin selection.
    pub(in crate::store) fn load_for_family(
        root: &Path,
        family: &crate::store::family::Family,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        Self::load_inner(root, Some(family), preparation)
    }

    fn load_inner(
        root: &Path,
        actual_family: Option<&crate::store::family::Family>,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        let info = opening_origin_info(root, preparation)?;
        if let Some(family) = actual_family {
            family.check_origin_selection(info.map(|(digest, _)| digest))?;
        }
        let Some((expected_digest, format)) = info else {
            return Ok(None);
        };
        if format == 1 {
            // Format 1 is metadata-only and never authorizes imported physical
            // bases or WAL intervals.
            return Ok(None);
        }
        let family = documents::Opened::root(root, preparation)?;
        let history = family.child("evidence-history", true, preparation)?;
        let encoded = history
            .child("origin.json", false, preparation)?
            .bytes(None, preparation)?;
        if Sha256Digest::of_bytes(&encoded.value, preparation.control())? != expected_digest {
            return Err(managed_history::unavailable(
                "origin changed while loading imported closure",
            ));
        }
        let origin = Origin::decode(&encoded.value, preparation)?;
        let imports = history.child("imports", true, preparation)?;
        let count = origin.value.sources.len();
        let held = preparation.reserve(
            (std::mem::size_of::<Self>() as u64 + 512)
                .checked_add(
                    (count as u64)
                        .checked_mul((std::mem::size_of::<DiskNode>() + 128) as u64)
                        .ok_or_else(|| preparation.exhausted())?,
                )
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(count)
            .map_err(|_| preparation.exhausted())?;
        for digest in &origin.value.sources {
            preparation.check()?;
            let text = digest.to_string();
            let name = text.strip_prefix("sha256:").expect("canonical digest");
            let directory = imports.child(name, true, preparation)?;
            let desc_bytes = directory
                .child("origin.json", false, preparation)?
                .bytes(None, preparation)?;
            if Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())? != *digest {
                return Err(managed_history::unavailable(
                    "imported source descriptor digest differs",
                ));
            }
            let descriptor = Descriptor::decode(&desc_bytes.value, preparation)?;
            for path in descriptor.value.files.keys() {
                drop(documents::read_member(
                    &directory,
                    &descriptor.value,
                    path,
                    preparation,
                )?);
            }
            documents::source_identities(&directory, &descriptor.value, preparation)?;
            let registry = documents::read_member(
                &directory,
                &descriptor.value,
                crate::managed::FILE,
                preparation,
            )?;
            let publication = primary::recorded_root_bytes(&registry.value, preparation)?
                .ok_or_else(|| {
                    managed_history::unavailable("imported source publication is missing")
                })?;
            drop(registry);
            drop(documents::source_publication(
                &directory,
                &descriptor.value,
                publication.identity(descriptor.value.family_id),
                preparation,
            )?);
            nodes.push(DiskNode {
                digest: *digest,
                descriptor,
                directory,
            });
        }
        check_acyclic(&origin.value.sources, &nodes, preparation)?;
        if let Some(family) = actual_family {
            family.check_origin_selection(Some(expected_digest))?;
        }
        preparation.check()?;
        Ok(Some(Self {
            origin_digest: expected_digest,
            history_dir: history,
            origin,
            nodes,
            _held: held,
        }))
    }

    fn source_index(&self, digest: Sha256Digest) -> Result<usize> {
        self.nodes
            .binary_search_by(|node| node.digest.as_bytes().cmp(digest.as_bytes()))
            .map_err(|_| managed_history::unavailable("imported assignment source is missing"))
    }

    /// Authenticate the destination publication recorded in `origin.json` format 2
    /// against `root`'s `managed.json`, `coverage.json`, and genesis generation.
    pub(in crate::store) fn verify_destination(
        &self,
        root: &Path,
        family: Uuid,
        history: &managed_history::Root,
        preparation: &Preparation,
    ) -> Result<()> {
        let Some(destination) = &self.origin.value.destination else {
            return Err(managed_history::unavailable(
                "format 2 origin is missing destination publication",
            ));
        };
        destination.validate(family)?;
        if self.origin.value.dataset_id != family
            || history.lineage_id != destination.genesis.lineage_id
            || history.coverage != destination.coverage
        {
            return Err(managed_history::unavailable(
                "destination origin disagrees with published registry lineage or coverage",
            ));
        }
        let coverage = managed_history::read_coverage(root, family, history, preparation)?;
        if coverage.value.activation_generation != destination.genesis.generation {
            return Err(managed_history::unavailable(
                "destination genesis differs from coverage activation generation",
            ));
        }
        drop(coverage);
        let genesis =
            managed_history::resolve(root, family, history, destination.genesis, preparation)?;
        if genesis.value.ordinal != 1 || genesis.value.previous.is_some() {
            return Err(managed_history::unavailable(
                "destination origin does not name genesis generation",
            ));
        }
        preparation.check()
    }

    /// Verify that `identity` is a genuine published configuration generation in
    /// one of the declared imported sources.
    pub(in crate::store) fn verify_identity(
        &self,
        identity: ConfigurationIdentity,
        preparation: &Preparation,
    ) -> Result<()> {
        for node in &self.nodes {
            preparation.check()?;
            if node.descriptor.value.family_id == identity.dataset_id {
                let _paths = preparation.reserve(1024)?;
                let path = format!(
                    "evidence-history/{}/generations/{}.json",
                    identity.lineage_id,
                    identity
                        .generation
                        .to_string()
                        .strip_prefix("sha256:")
                        .expect("canonical digest")
                );
                if node.descriptor.value.files.contains_key(&path) {
                    drop(documents::source_publication(
                        &node.directory,
                        &node.descriptor.value,
                        identity,
                        preparation,
                    )?);
                    return preparation.check();
                }
            }
        }
        Err(managed_history::unavailable(
            "imported configuration identity is not in any declared original source",
        ))
    }

    /// Authenticate a reachable prior source publication starting from `start`.
    fn verify_prior_from(
        &self,
        start: usize,
        identity: ConfigurationIdentity,
        preparation: &Preparation,
    ) -> Result<()> {
        let count = self.nodes.len();
        let _held = preparation.reserve(
            (count as u64)
                .checked_mul((std::mem::size_of::<usize>() + 1) as u64)
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut visited = vec![false; count];
        let mut pending = Vec::new();
        visited[start] = true;
        for digest in &self.nodes[start].descriptor.value.sources {
            let next = self.source_index(*digest)?;
            if !visited[next] {
                visited[next] = true;
                pending.push(next);
            }
        }
        while let Some(index) = pending.pop() {
            preparation.check()?;
            let node = &self.nodes[index];
            if node.descriptor.value.family_id == identity.dataset_id {
                let _paths = preparation.reserve(1024)?;
                let path = format!(
                    "evidence-history/{}/generations/{}.json",
                    identity.lineage_id,
                    identity
                        .generation
                        .to_string()
                        .strip_prefix("sha256:")
                        .expect("canonical digest")
                );
                if node.descriptor.value.files.contains_key(&path) {
                    drop(documents::source_publication(
                        &node.directory,
                        &node.descriptor.value,
                        identity,
                        preparation,
                    )?);
                    return preparation.check();
                }
            }
            for digest in &node.descriptor.value.sources {
                let next = self.source_index(*digest)?;
                if !visited[next] {
                    visited[next] = true;
                    pending.push(next);
                }
            }
        }
        Err(managed_history::unavailable(
            "imported base publication is not in its reachable original sources",
        ))
    }

    /// Check the creation origin exception (if `actual` is a clone's `seq == 0`
    /// creation commit matching `destination.creation_commit`).
    pub(in crate::store) fn creation_origin(
        &self,
        actual: &RetainedGenerationCommit,
        preparation: &Preparation,
    ) -> Result<Option<(u64, ConfigurationBinding)>> {
        match &self.origin.value.destination {
            Some(destination) => destination.creation_origin(actual, preparation),
            None => Ok(None),
        }
    }

    /// Authenticate a generation's `commit.json` and `meta.json` in read-only copy
    /// validation (`validation.rs`), returning the native `primary::Header` to use
    /// for native commits in the generation's WAL.
    pub(in crate::store) fn check_generation_base(
        &self,
        family_root: &Path,
        gen_dir: &Path,
        gen_name: &str,
        actual: &RetainedGenerationCommit,
        preparation: &Preparation,
    ) -> Result<primary::Header> {
        let creation = self.creation_origin(actual, preparation)?;
        let mut native = primary::Header::on_disk(
            family_root,
            self.origin.value.dataset_id,
            actual.dataset_id,
            creation,
            preparation,
        )?;
        let local = Location {
            branch: None,
            generation: gen_name.to_owned(),
        };
        local.validate()?;
        if let Some(base) = self
            .origin
            .value
            .bases
            .iter()
            .find(|base| base.local == local)
        {
            let index = self.source_index(base.source)?;
            let node = &self.nodes[index];
            let root_name = base
                .original
                .branch
                .map_or_else(|| "main".to_owned(), |id| id.to_string());
            let orig =
                read_disk_original_base(node, &root_name, &base.original.generation, preparation)?;
            let actual_meta = read_meta_controlled(&gen_dir.join("meta.json"), preparation)?;
            let retagged = match orig.physical.configuration {
                ConfigurationBinding::Known { configuration } => {
                    ConfigurationBinding::ImportedUnbound {
                        source: configuration,
                    }
                }
                other => other,
            };
            if orig.meta != actual_meta.value
                || orig.physical.commit != actual.commit
                || orig.physical.origin != actual.origin
                || (actual.configuration != orig.physical.configuration
                    && actual.configuration != retagged)
            {
                return Err(managed_history::unavailable(
                    "imported original physical base differs from copied generation",
                ));
            }
            let orig_seq = orig.physical.commit.seq;
            let orig_config = orig.physical.configuration;
            drop(actual_meta);
            drop(orig);
            let mut header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &root_name,
                preparation,
            )?;
            match orig_config {
                ConfigurationBinding::ImportedUnbound { source } => {
                    self.verify_prior_from(index, source, preparation)?;
                }
                binding => {
                    header.generation_using(preparation, orig_seq, binding, &mut |identity| {
                        documents::source_publication(
                            &node.directory,
                            &node.descriptor.value,
                            identity,
                            preparation,
                        )
                        .map(|_| ())
                    })?
                }
            }
            if matches!(actual.configuration, ConfigurationBinding::Known { .. }) {
                native.generation(
                    family_root,
                    preparation,
                    actual.commit.seq,
                    actual.configuration,
                )?;
            }
        } else if let ConfigurationBinding::ImportedUnbound { source } = actual.configuration {
            if !native.is_pre_cutoff(actual.commit.seq) {
                return Err(managed_history::unavailable(
                    "covered generation cannot hold an imported configuration binding",
                ));
            }
            self.verify_identity(source, preparation)?;
        } else if matches!(actual.configuration, ConfigurationBinding::LegacyUnbound) {
            if creation.is_some() || !native.is_pre_cutoff(actual.commit.seq) {
                native.generation(
                    family_root,
                    preparation,
                    actual.commit.seq,
                    actual.configuration,
                )?;
                return Err(managed_history::unavailable(
                    "imported legacy base is missing its original physical assignment",
                ));
            }
            self.verify_legacy_unbound_base(actual.commit.seq, preparation)?;
        } else {
            native.generation(
                family_root,
                preparation,
                actual.commit.seq,
                actual.configuration,
            )?;
        }
        Ok(native)
    }

    fn verify_legacy_unbound_base(&self, seq: u64, preparation: &Preparation) -> Result<()> {
        let mut covered_by_source = false;
        for node in &self.nodes {
            preparation.check()?;
            let Some(main_root) = node.descriptor.value.roots.get("main") else {
                continue;
            };
            if main_root.head < seq {
                continue;
            }
            covered_by_source = true;
            let mut header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                "main",
                preparation,
            )?;
            header.generation_using(
                preparation,
                seq,
                ConfigurationBinding::LegacyUnbound,
                &mut |_| Ok(()),
            )?;
        }
        if !covered_by_source {
            return Err(managed_history::unavailable(
                "imported legacy base has no declared source covering its sequence",
            ));
        }
        preparation.check()
    }

    /// Return whether `gen_name` declares any imported WAL intervals.
    pub(in crate::store) fn has_ranges(&self, gen_name: &str) -> bool {
        self.origin
            .value
            .ranges
            .iter()
            .any(|r| r.local.branch.is_none() && r.local.generation == gen_name)
    }

    /// Prove a copied WAL against both its declared imported intervals and native
    /// suffix (`validation.rs` and retained cold reads). When `through < u64::MAX`,
    /// scopes the proof to `through` and every complete imported interval starting
    /// at or before `through`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::store) fn prove_wal<R: Read + Seek>(
        &self,
        family_root: &Path,
        gen_dir: &Path,
        gen_name: &str,
        input: R,
        extent: u64,
        base: u64,
        through: u64,
        terms: u64,
        annotated: Option<&[u64]>,
        wal_path: &Path,
        native: &mut primary::Header,
        budget: &mut ColdBudget<'_>,
    ) -> Result<()> {
        let start = budget.used();
        let result = (|| -> Result<()> {
            prove_wal_stage_hook("entered", budget.used(), budget.available(), None, None);
            let local = Location {
                branch: None,
                generation: gen_name.to_owned(),
            };
            struct DiskRange {
                span: physical::Span,
                source: usize,
                header: primary::Header,
            }
            let actual_commit_loaded = crate::commit::read_gen_commit_with(gen_dir, budget)?
                .ok_or_else(|| {
                    managed_history::unavailable("copied generation commit is missing")
                })?;

            let count = self
                .origin
                .value
                .ranges
                .iter()
                .filter(|r| r.local == local)
                .count();
            let range_storage = (count as u64)
                .checked_mul(
                    (std::mem::size_of::<DiskRange>() + std::mem::size_of::<physical::Span>())
                        as u64,
                )
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| budget.exhausted())?;
            budget.charge(range_storage)?;
            let mut ranges: Vec<DiskRange> = Vec::new();
            ranges
                .try_reserve_exact(count)
                .map_err(|_| budget.exhausted())?;

            // Setup reads use a temporary Preparation bounded by `budget.available()`
            // while `budget` retains `actual_commit_loaded` and `range_storage` and is
            // not mutated. Release setup allocations and `actual_commit_loaded` before
            // physical proof starts.
            {
                let setup_prep = budget
                    .owned_limit()?
                    .options
                    .temporary(budget.available())?;
                let actual_meta = read_meta_controlled(&gen_dir.join("meta.json"), &setup_prep)?;
                prove_wal_stage_hook(
                    "setup_meta",
                    budget.used(),
                    budget.available(),
                    Some(setup_prep.limit()),
                    Some(setup_prep.used()),
                );
                for assignment in &self.origin.value.ranges {
                    setup_prep.check()?;
                    if assignment.local != local {
                        continue;
                    }
                    let index = self.source_index(assignment.source)?;
                    let node = &self.nodes[index];
                    let interval = node
                        .descriptor
                        .value
                        .intervals
                        .get(assignment.interval as usize)
                        .ok_or_else(|| {
                            managed_history::unavailable(
                                "imported range has no declared source interval",
                            )
                        })?;
                    let orig = read_disk_original_base(
                        node,
                        &interval.root,
                        &interval.generation,
                        &setup_prep,
                    )?;
                    prove_wal_stage_hook(
                        "setup_source",
                        budget.used(),
                        budget.available(),
                        Some(setup_prep.limit()),
                        Some(setup_prep.used()),
                    );
                    let retagged = match orig.physical.configuration {
                        ConfigurationBinding::Known { configuration } => {
                            ConfigurationBinding::ImportedUnbound {
                                source: configuration,
                            }
                        }
                        other => other,
                    };
                    if orig.meta != actual_meta.value
                        || orig.physical.commit != actual_commit_loaded.value.commit
                        || orig.physical.origin != actual_commit_loaded.value.origin
                        || (actual_commit_loaded.value.configuration != orig.physical.configuration
                            && actual_commit_loaded.value.configuration != retagged)
                    {
                        return Err(managed_history::unavailable(
                            "imported WAL range base differs from copied generation",
                        ));
                    }
                    drop(orig);
                    let header = documents::source_header(
                        &node.directory,
                        &node.descriptor.value,
                        &interval.root,
                        &setup_prep,
                    )?;
                    ranges.push(DiskRange {
                        span: physical::Span {
                            start: interval.start,
                            end: interval.end,
                            sha256: interval.sha256,
                            first_seq: interval.first_seq,
                            last_seq: interval.last_seq,
                        },
                        source: index,
                        header,
                    });
                }
                drop(actual_meta);
            }
            let actual_commit_charge = actual_commit_loaded.charge;
            drop(actual_commit_loaded.value);
            budget.release(actual_commit_charge);
            ranges.sort_unstable_by_key(|r| r.span.start);
            if ranges.windows(2).any(|p| p[0].span.end > p[1].span.start) {
                return Err(managed_history::unavailable(
                    "ambiguous imported physical intervals",
                ));
            }
            if through < u64::MAX {
                ranges.retain(|r| r.span.first_seq <= through);
            }
            let mut spans: Vec<physical::Span> = Vec::new();
            spans
                .try_reserve_exact(ranges.len())
                .map_err(|_| budget.exhausted())?;
            for r in &ranges {
                spans.push(physical::Span {
                    start: r.span.start,
                    end: r.span.end,
                    sha256: r.span.sha256,
                    first_seq: r.span.first_seq,
                    last_seq: r.span.last_seq,
                });
            }
            prove_wal_stage_hook(
                "before_physical",
                budget.used(),
                budget.available(),
                None,
                None,
            );
            let mut config_error = None;
            let mut visit = |observation: wal::probe::WalCommitObservation,
                             proof_budget: &ColdBudget<'_>| {
                if config_error.is_none() {
                    let matched = ranges.iter_mut().find(|r| {
                        (r.span.start <= observation.start_offset
                            || (r.span.start == 0 && observation.start_offset == WAL_REC as u64))
                            && observation.point.offset <= r.span.end
                    });
                    let result = if let Some(range) = matched {
                        let node = &self.nodes[range.source];
                        let callback_prep = proof_budget
                            .owned_limit()?
                            .options
                            .temporary(proof_budget.available())?;
                        prove_wal_stage_hook(
                            "callback_imported",
                            proof_budget.used(),
                            proof_budget.available(),
                            Some(callback_prep.limit()),
                            Some(callback_prep.used()),
                        );
                        range.header.observation_using(
                            &callback_prep,
                            observation,
                            &mut |identity| {
                                documents::source_publication(
                                    &node.directory,
                                    &node.descriptor.value,
                                    identity,
                                    &callback_prep,
                                )
                                .map(|_| ())
                            },
                        )
                    } else {
                        match native.cached_observation(observation) {
                            Ok(true) => Ok(()),
                            Ok(false) => {
                                let callback_prep = proof_budget
                                    .owned_limit()?
                                    .options
                                    .temporary(proof_budget.available())?;
                                prove_wal_stage_hook(
                                    "callback_native",
                                    proof_budget.used(),
                                    proof_budget.available(),
                                    Some(callback_prep.limit()),
                                    Some(callback_prep.used()),
                                );
                                native.observation(family_root, &callback_prep, observation)
                            }
                            Err(error) => Err(error),
                        }
                    };
                    if let Err(error) = result {
                        if matches!(
                            error,
                            Error::BudgetExceeded(_) | Error::Cancelled | Error::Timeout
                        ) {
                            return Err(error);
                        }
                        config_error = Some(error);
                    }
                }
                Ok(())
            };
            if through == u64::MAX {
                let _ = physical::prove_prefix(
                    input,
                    &spans,
                    extent,
                    base,
                    u64::MAX,
                    terms,
                    annotated,
                    wal_path,
                    budget,
                    &mut visit,
                )?;
            } else {
                let proof_target = spans.last().map_or(through, |s| through.max(s.last_seq));
                let _ = physical::prove_prefix_through(
                    input,
                    &spans,
                    extent,
                    base,
                    proof_target,
                    terms,
                    annotated,
                    wal_path,
                    budget,
                    &mut visit,
                )?;
            }
            if let Some(error) = config_error {
                return Err(error);
            }
            Ok(())
        })();
        budget.release(budget.used() - start);
        result
    }
}

struct DiskOriginalBase {
    physical: RetainedGenerationCommit,
    meta: crate::builder::IndexMeta,
    _held: Reservation,
}

fn read_disk_original_base(
    node: &DiskNode,
    root_name: &str,
    generation: &str,
    preparation: &Preparation,
) -> Result<DiskOriginalBase> {
    let _paths = preparation.reserve(
        (root_name.len() as u64)
            .saturating_mul(4)
            .saturating_add(4096),
    )?;
    let prefix = if root_name == "main" {
        String::new()
    } else {
        format!("branches/{root_name}/")
    };
    let commit = documents::read_member(
        &node.directory,
        &node.descriptor.value,
        &format!("{prefix}{generation}/commit.json"),
        preparation,
    )?;
    let meta_bytes = documents::read_member(
        &node.directory,
        &node.descriptor.value,
        &format!("{prefix}{generation}/meta.json"),
        preparation,
    )?;
    let allowance = preparation.room();
    let mut held = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let physical = crate::commit::decode_gen_commit_bytes_with(
        Path::new("original-primary/commit.json"),
        &commit.value,
        &mut budget,
    )?;
    let meta: crate::store::cold::Loaded<crate::builder::IndexMeta> =
        budget.json(&meta_bytes.value)?;
    let logical = node
        .descriptor
        .value
        .roots
        .get(root_name)
        .ok_or_else(|| managed_history::unavailable("original base has no logical root"))?;
    if physical.value.dataset_id != logical.logical_id {
        return Err(managed_history::unavailable(
            "original base belongs to another logical dataset",
        ));
    }
    drop(commit);
    drop(meta_bytes);
    held.release_to(budget.used());
    preparation.check()?;
    Ok(DiskOriginalBase {
        physical: physical.value,
        meta: meta.value,
        _held: held,
    })
}

pub(super) fn read_meta_controlled(
    path: &Path,
    preparation: &Preparation,
) -> Result<managed_history::Owned<crate::builder::IndexMeta>> {
    let bytes = managed_history::read_bytes(path, preparation)?;
    let allowance = preparation.room();
    let mut held = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let meta: crate::store::cold::Loaded<crate::builder::IndexMeta> =
        budget.json(&bytes.value).map_err(|e| match e {
            Error::BudgetExceeded(_) | Error::Cancelled | Error::Timeout => e,
            _ => managed_history::unavailable("invalid copied meta.json"),
        })?;
    drop(bytes);
    held.release_to(budget.used());
    preparation.check()?;
    Ok(managed_history::Owned::new(meta.value, held))
}

fn check_acyclic(
    sources: &[Sha256Digest],
    nodes: &[DiskNode],
    preparation: &Preparation,
) -> Result<()> {
    if !sorted_sources(sources) {
        return Err(managed_history::unavailable(
            "imported sources are not unique and sorted",
        ));
    }
    let count = sources.len();
    let bytes = count
        .checked_mul(std::mem::size_of::<(usize, usize)>() + 1)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(1024))
        .ok_or_else(|| preparation.exhausted())?;
    let _held = preparation.reserve(bytes as u64)?;
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    colors.resize(count, 0u8);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    stack
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    for start in 0..count {
        preparation.check()?;
        if colors[start] == 2 {
            continue;
        }
        colors[start] = 1;
        stack.push((start, 0));
        while let Some((index, next)) = stack.last_mut() {
            preparation.check()?;
            let edges = &nodes[*index].descriptor.value.sources;
            if *next == edges.len() {
                colors[*index] = 2;
                stack.pop();
                continue;
            }
            let digest = edges[*next];
            *next += 1;
            let target = sources
                .binary_search_by(|id| id.as_bytes().cmp(digest.as_bytes()))
                .map_err(|_| {
                    managed_history::unavailable("imported source dependency is missing")
                })?;
            match colors[target] {
                1 => {
                    return Err(managed_history::unavailable(
                        "imported source dependency cycle",
                    ));
                }
                0 => {
                    colors[target] = 1;
                    stack.push((target, 0));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Verify that an `ImportedUnbound` identity is published in one of `root`'s
/// declared imported sources.
pub(in crate::store) fn verify_imported_identity(
    root: &Path,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<()> {
    let Some(closure) = DiskClosure::load(root, preparation)? else {
        return Err(managed_history::unavailable(
            "imported configuration identity has no origin closure",
        ));
    };
    closure.verify_identity(identity, preparation)
}

/// Verify an `ImportedUnbound` identity against `root`'s closure while checking
/// that `family` has a destination origin matching `origin.json` on disk.
pub(in crate::store) fn verify_imported_identity_for_family(
    root: &Path,
    family: &crate::store::family::Family,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<()> {
    if !family.has_destination_origin() {
        return Err(managed_history::unavailable(
            "imported configuration identity has no destination origin",
        ));
    }
    let Some(closure) = DiskClosure::load_for_family(root, family, preparation)? else {
        return Err(managed_history::unavailable(
            "imported configuration identity has no origin closure",
        ));
    };
    closure.verify_identity(identity, preparation)
}

fn closure_import_file_count(closure: &DiskClosure) -> Option<usize> {
    let mut total = 1usize; // evidence-history/origin.json
    for node in &closure.nodes {
        total = total
            .checked_add(1)?
            .checked_add(node.descriptor.value.files.len())?;
    }
    Some(total)
}

fn check_manifest_file_budget(
    existing_files: usize,
    additional_files: Option<usize>,
) -> Result<()> {
    let total = additional_files.and_then(|n| existing_files.checked_add(n));
    if total.is_none_or(|t| t >= managed_history::capture::MAX_FILES) {
        return Err(managed_history::unavailable(
            "configuration closure exceeds backup manifest limits",
        ));
    }
    Ok(())
}

/// Capture `evidence-history/origin.json` and every declared file under
/// `evidence-history/imports/` as anchored-FD-pinned, hash-verified `CapturedFile`s
/// for backup creation, without reopening verified paths outside the anchored dirs.
pub(in crate::store) fn capture_import_files(
    root: &Path,
    existing_files: usize,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    let Some(closure) = DiskClosure::load(root, preparation)? else {
        return Ok(Vec::new());
    };
    check_manifest_file_budget(existing_files, closure_import_file_count(&closure))?;
    let mut out = Vec::new();
    let origin_file = closure
        .history_dir
        .child("origin.json", false, preparation)?
        .into_pinned_file(closure.origin_digest, preparation, control)?;
    out.push(CapturedFile {
        path: ROOT_FILE.to_owned(),
        kind: FileKind::Immutable,
        len: origin_file.metadata()?.len(),
        src: FileSource::File(origin_file),
    });
    pin_closure_import_members(&closure, existing_files, &mut out, preparation, control)?;
    Ok(out)
}

fn pin_closure_import_members(
    closure: &DiskClosure,
    existing_files: usize,
    out: &mut Vec<CapturedFile>,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<()> {
    for node in &closure.nodes {
        control.check()?;
        if existing_files.saturating_add(out.len()).saturating_add(1)
            >= managed_history::capture::MAX_FILES
        {
            return Err(managed_history::unavailable(
                "configuration closure exceeds backup manifest limits",
            ));
        }
        let hex = node.digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        let desc_rel = format!("evidence-history/imports/{hex}/origin.json");
        let desc_file = node
            .directory
            .child("origin.json", false, preparation)?
            .into_pinned_file(node.digest, preparation, control)?;
        out.push(CapturedFile {
            path: desc_rel,
            kind: FileKind::Immutable,
            len: desc_file.metadata()?.len(),
            src: FileSource::File(desc_file),
        });
        let source_dir = node.directory.child("source", true, preparation)?;
        for (member_path, pin) in &node.descriptor.value.files {
            control.check()?;
            if existing_files.saturating_add(out.len()).saturating_add(1)
                >= managed_history::capture::MAX_FILES
            {
                return Err(managed_history::unavailable(
                    "configuration closure exceeds backup manifest limits",
                ));
            }
            let rel = format!("evidence-history/imports/{hex}/source/{member_path}");
            let file = source_dir
                .relative(member_path, false, preparation)?
                .into_pinned_file(pin.sha256, preparation, control)?;
            out.push(CapturedFile {
                path: rel,
                kind: FileKind::Immutable,
                len: file.metadata()?.len(),
                src: FileSource::File(file),
            });
        }
    }
    Ok(())
}

/// Capture reconciled imported provenance for a materialized backup of a
/// persistent main store of an imported family. Copies `evidence-history/imports/`
/// via anchored FDs and writes a reconciled format 2 `evidence-history/origin.json`
/// with empty `bases` and `ranges` reflecting the freshly materialized `gen-0001`.
pub(in crate::store) fn capture_materialized_imported_origin(
    family_root: &Path,
    family: &crate::store::family::Family,
    staging_dir: &Path,
    existing_files: usize,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    let Some(closure) = DiskClosure::load_for_family(family_root, family, preparation)? else {
        return Ok(Vec::new());
    };
    check_manifest_file_budget(existing_files, closure_import_file_count(&closure))?;
    let reconciled =
        Origin {
            format: 2,
            dataset_id: closure.origin.value.dataset_id,
            destination: closure.origin.value.destination.as_ref().map(|d| {
                destination::Publication {
                    genesis: d.genesis,
                    coverage: d.coverage,
                    creation_commit: d.creation_commit,
                }
            }),
            sources: closure.origin.value.sources.clone(),
            bases: Vec::new(),
            ranges: Vec::new(),
        };
    reconciled.validate(preparation)?;
    let origin_bytes = managed_history::encode(&reconciled, preparation)?;
    let origin_digest = Sha256Digest::of_bytes(&origin_bytes.value, preparation.control())?;
    let origin_target = crate::store::definitions::stage_file(staging_dir, ROOT_FILE)?;
    crate::store::write_synced(&origin_target, &origin_bytes.value)?;
    let origin_file =
        managed_history::capture::pinned_file(&origin_target, origin_digest, preparation, control)?;
    let mut out = Vec::new();
    out.push(CapturedFile {
        path: ROOT_FILE.to_owned(),
        kind: FileKind::Immutable,
        len: origin_file.metadata()?.len(),
        src: FileSource::File(origin_file),
    });
    pin_closure_import_members(&closure, existing_files, &mut out, preparation, control)?;
    Ok(out)
}

/// Emit format 2 `evidence-history/origin.json` (with `creation_commit`) and
/// `evidence-history/imports/...` into `staging_dir` for a materialized backup
/// of an in-memory clone, returning the resulting FD-pinned `CapturedFile`s.
#[allow(clippy::too_many_arguments)]
pub(in crate::store) fn write_memory_clone_origin(
    staging_dir: &Path,
    clone_id: Uuid,
    genesis: ConfigurationIdentity,
    coverage_digest: Sha256Digest,
    memory_origin: &managed_history::MemoryOrigin,
    existing_files: usize,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    preparation.check()?;
    let planned_files = (|| -> Option<usize> {
        // origin.json + immediate source descriptor + immediate source files + prior imports
        let mut total = 2usize.checked_add(memory_origin.source_files.len())?;
        for (_, files) in &memory_origin.prior_imports {
            total = total.checked_add(files.len())?;
        }
        Some(total)
    })();
    check_manifest_file_budget(existing_files, planned_files)?;
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut staged_paths: Vec<(String, Sha256Digest)> = Vec::new();
    for (prior_digest, files) in &memory_origin.prior_imports {
        all_sources.insert(*prior_digest.as_bytes());
        let hex = prior_digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        for (rel, bytes) in files {
            control.check()?;
            preparation.check()?;
            let path = format!("evidence-history/imports/{hex}/{rel}");
            let target = crate::store::definitions::stage_file(staging_dir, &path)?;
            crate::store::write_synced(&target, &bytes.value)?;
            let digest = Sha256Digest::of_bytes(&bytes.value, preparation.control())?;
            staged_paths.push((path, digest));
        }
    }
    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &memory_origin.source_files {
        control.check()?;
        preparation.check()?;
        if !source_member(rel) {
            return Err(managed_history::unavailable(
                "memory clone source provenance contains an invalid member path",
            ));
        }
        let pin = SourceFile {
            bytes: bytes.value.len() as u64,
            sha256: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
        };
        if file_pins.insert(rel.clone(), pin).is_some() {
            return Err(managed_history::unavailable(
                "memory clone source provenance repeats a member",
            ));
        }
    }
    let mut direct_sources = memory_origin.prior_direct_sources.clone();
    direct_sources.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    direct_sources.dedup();
    let descriptor = Descriptor {
        format: FORMAT,
        family_id: memory_origin.src_family,
        roots: BTreeMap::from([(
            "main".to_owned(),
            SourceRoot {
                logical_id: memory_origin.src_logical,
                head: memory_origin.src_head,
                generation: memory_origin.src_generation.clone(),
            },
        )]),
        files: file_pins,
        sources: direct_sources,
        intervals: Vec::new(),
    };
    descriptor.validate(preparation)?;
    let desc_bytes = managed_history::encode(&descriptor, preparation)?;
    let src_digest = Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())?;
    let src_hex = src_digest.to_string();
    let src_hex = src_hex.strip_prefix("sha256:").expect("canonical digest");
    for (rel, bytes) in &memory_origin.source_files {
        control.check()?;
        preparation.check()?;
        let path = format!("evidence-history/imports/{src_hex}/source/{rel}");
        let target = crate::store::definitions::stage_file(staging_dir, &path)?;
        crate::store::write_synced(&target, &bytes.value)?;
        let digest = descriptor.files[rel].sha256;
        staged_paths.push((path, digest));
    }
    let desc_rel = format!("evidence-history/imports/{src_hex}/origin.json");
    let desc_target = crate::store::definitions::stage_file(staging_dir, &desc_rel)?;
    crate::store::write_synced(&desc_target, &desc_bytes.value)?;
    staged_paths.push((desc_rel, src_digest));
    all_sources.insert(*src_digest.as_bytes());

    let origin = Origin {
        format: 2,
        dataset_id: clone_id,
        destination: Some(destination::Publication {
            genesis,
            coverage: coverage_digest,
            creation_commit: Some(memory_origin.creation_commit),
        }),
        sources: all_sources
            .into_iter()
            .map(Sha256Digest::from_digest_bytes)
            .collect(),
        bases: Vec::new(),
        ranges: Vec::new(),
    };
    origin.validate(preparation)?;
    let origin_bytes = managed_history::encode(&origin, preparation)?;
    let origin_digest = Sha256Digest::of_bytes(&origin_bytes.value, preparation.control())?;
    let origin_target = crate::store::definitions::stage_file(staging_dir, ROOT_FILE)?;
    crate::store::write_synced(&origin_target, &origin_bytes.value)?;

    let mut out = Vec::with_capacity(staged_paths.len() + 1);
    let origin_file =
        managed_history::capture::pinned_file(&origin_target, origin_digest, preparation, control)?;
    out.push(CapturedFile {
        path: ROOT_FILE.to_owned(),
        kind: FileKind::Immutable,
        len: origin_file.metadata()?.len(),
        src: FileSource::File(origin_file),
    });
    for (rel, digest) in staged_paths {
        control.check()?;
        let file = managed_history::capture::pinned_file(
            &staging_dir.join(&rel),
            digest,
            preparation,
            control,
        )?;
        out.push(CapturedFile {
            path: rel,
            kind: FileKind::Immutable,
            len: file.metadata()?.len(),
            src: FileSource::File(file),
        });
    }
    Ok(out)
}

/// Compute the canonical creation-commit digest for a clone's `seq == 0` root.
pub(in crate::store) fn clone_creation_digest(
    dataset_id: Uuid,
    root_commit: &CommitInfo,
    genesis: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<Sha256Digest> {
    destination::creation_digest(
        dataset_id,
        root_commit,
        ConfigurationBinding::Known {
            configuration: genesis,
        },
        preparation,
    )
}

/// Materialize an in-memory clone's retained [`managed_history::MemoryOrigin`]
/// into `(origin_json_bytes, prior_imports, prior_direct_sources)` so downstream
/// clones and branch backups preserve the complete ancestor provenance DAG.
pub(in crate::store) fn materialize_memory_origin_imports(
    family_id: Uuid,
    installed: &Installed,
    memory: &managed_history::MemoryHistory,
    memory_origin: &managed_history::MemoryOrigin,
    preparation: &Preparation,
) -> Result<(
    managed_history::Bytes,
    Vec<PriorImportFiles>,
    Vec<Sha256Digest>,
)> {
    preparation.check()?;
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut prior_imports: Vec<PriorImportFiles> = Vec::new();
    prior_imports
        .try_reserve_exact(memory_origin.prior_imports.len() + 1)
        .map_err(|_| preparation.exhausted())?;
    for (prior_digest, files) in &memory_origin.prior_imports {
        all_sources.insert(*prior_digest.as_bytes());
        let mut copied_files = Vec::new();
        copied_files
            .try_reserve_exact(files.len())
            .map_err(|_| preparation.exhausted())?;
        for (rel, bytes) in files {
            preparation.check()?;
            copied_files.push((
                rel.clone(),
                managed_history::Bytes::copy_from_slice(&bytes.value, preparation)?,
            ));
        }
        prior_imports.push((*prior_digest, copied_files));
    }

    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &memory_origin.source_files {
        preparation.check()?;
        if !source_member(rel) {
            return Err(managed_history::unavailable(
                "memory clone source provenance contains an invalid member path",
            ));
        }
        let pin = SourceFile {
            bytes: bytes.value.len() as u64,
            sha256: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
        };
        if file_pins.insert(rel.clone(), pin).is_some() {
            return Err(managed_history::unavailable(
                "memory clone source provenance repeats a member",
            ));
        }
    }
    let mut direct_sources = memory_origin.prior_direct_sources.clone();
    direct_sources.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    direct_sources.dedup();
    let descriptor = Descriptor {
        format: FORMAT,
        family_id: memory_origin.src_family,
        roots: BTreeMap::from([(
            "main".to_owned(),
            SourceRoot {
                logical_id: memory_origin.src_logical,
                head: memory_origin.src_head,
                generation: memory_origin.src_generation.clone(),
            },
        )]),
        files: file_pins,
        sources: direct_sources,
        intervals: Vec::new(),
    };
    descriptor.validate(preparation)?;
    let desc_bytes = managed_history::encode(&descriptor, preparation)?;
    let src_digest = Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())?;
    let mut immediate_files = Vec::new();
    immediate_files
        .try_reserve_exact(memory_origin.source_files.len() + 1)
        .map_err(|_| preparation.exhausted())?;
    immediate_files.push(("origin.json".to_owned(), desc_bytes));
    for (rel, bytes) in &memory_origin.source_files {
        preparation.check()?;
        immediate_files.push((
            format!("source/{rel}"),
            managed_history::Bytes::copy_from_slice(&bytes.value, preparation)?,
        ));
    }
    prior_imports.push((src_digest, immediate_files));
    all_sources.insert(*src_digest.as_bytes());
    let sorted_sources: Vec<Sha256Digest> = all_sources
        .into_iter()
        .map(Sha256Digest::from_digest_bytes)
        .collect();

    let history = installed.configuration_history.as_ref().ok_or_else(|| {
        managed_history::unavailable("in-memory clone has no published configuration root")
    })?;
    let genesis = ConfigurationIdentity {
        dataset_id: family_id,
        lineage_id: history.lineage_id,
        generation: memory.coverage.value.activation_generation,
    };
    let origin = Origin {
        format: 2,
        dataset_id: family_id,
        destination: Some(destination::Publication {
            genesis,
            coverage: history.coverage,
            creation_commit: Some(memory_origin.creation_commit),
        }),
        sources: sorted_sources.clone(),
        bases: Vec::new(),
        ranges: Vec::new(),
    };
    origin.validate(preparation)?;
    let origin_bytes = managed_history::encode(&origin, preparation)?;
    Ok((origin_bytes, prior_imports, sorted_sources))
}

/// Branch identity documents captured under the branch writer lock so concurrent
/// compaction cannot shift `CURRENT` relative to the captured head and generation.
pub(in crate::store) struct BranchProvenanceCapture {
    pub(in crate::store) identities: super::Identities,
    pub(in crate::store) source_generation: String,
    pub(in crate::store) source_dataset: Option<managed_history::Bytes>,
    pub(in crate::store) source_current: Option<managed_history::Bytes>,
}

/// Anchored generation commit bytes and WAL extent captured once by direct
/// branch backup so the emitted retagged commit and the provenance source
/// member always come from the same read.
pub(in crate::store) struct DirectBranchWalInput<'a> {
    pub(in crate::store) gen_dir: &'a Path,
    pub(in crate::store) base_seq: u64,
    pub(in crate::store) wal_extent: u64,
    pub(in crate::store) commit_bytes: managed_history::Bytes,
}

/// Build standalone destination provenance for a public backup of a branch of an
/// activated family (materialized or direct unlinked). Establishes a fresh
/// destination lineage for `branch_id` with `first_bound_own = head_seq + 1` and
/// embeds the parent family's configuration closure + `branch.json` + any prior
/// imported sources under `evidence-history/imports/`.
#[allow(clippy::too_many_arguments)]
pub(in crate::store) fn build_branch_standalone_provenance(
    staging_dir: &Path,
    family_root: Option<&Path>,
    family: &crate::store::family::Family,
    branch_root: Option<&Path>,
    family_id: Uuid,
    branch_id: Uuid,
    head_seq: u64,
    dest_generation: &str,
    installed: &Installed,
    captured: &BranchProvenanceCapture,
    mut wal_interval_inputs: Option<DirectBranchWalInput<'_>>,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    preparation.check()?;
    let source_generation = captured.source_generation.as_str();
    if !generation(source_generation) || !generation(dest_generation) {
        return Err(managed_history::unavailable(
            "branch backup generation name is invalid",
        ));
    }
    if wal_interval_inputs.is_some() && source_generation != dest_generation {
        return Err(managed_history::unavailable(
            "direct branch backup source and destination generations differ",
        ));
    }
    if wal_interval_inputs.is_some() && (family_root.is_none() || branch_root.is_none()) {
        return Err(managed_history::unavailable(
            "direct branch backup WAL input requires persistent family and branch roots",
        ));
    }
    let mut wal_interval_meta: Option<(&Path, u64, u64)> = None;
    let mut source_files: BTreeMap<String, managed_history::Bytes> = BTreeMap::new();
    let mut push_src = |rel: String, bytes: managed_history::Bytes| -> Result<()> {
        if source_files.len() >= MAX_MEMBERS {
            return Err(managed_history::unavailable(
                "branch backup provenance exceeds member limit",
            ));
        }
        source_files.insert(rel, bytes);
        Ok(())
    };
    push_src(
        "family-dataset.json".into(),
        managed_history::Bytes::copy_from_slice(
            captured.identities.family_document(),
            preparation,
        )?,
    )?;
    if let Some(branch_bytes) = captured.identities.branch_document() {
        push_src(
            "branch.json".into(),
            managed_history::Bytes::copy_from_slice(branch_bytes, preparation)?,
        )?;
    }
    let (prior_imports, prior_direct) = if let Some(froot) = family_root {
        // Use the branch dataset.json and CURRENT captured under the writer lock
        // when persistent, and (if direct unlinked) reuse the single anchored
        // commit.json bytes alongside the leased generation's immutable meta.json.
        if let Some(broot) = branch_root {
            let ds_bytes = match &captured.source_dataset {
                Some(bytes) => managed_history::Bytes::copy_from_slice(&bytes.value, preparation)?,
                None => documents::Opened::root(broot, preparation)?
                    .child("dataset.json", false, preparation)?
                    .bytes(None, preparation)?,
            };
            let cur_bytes = match &captured.source_current {
                Some(bytes) => managed_history::Bytes::copy_from_slice(&bytes.value, preparation)?,
                None => documents::Opened::root(broot, preparation)?
                    .child("CURRENT", false, preparation)?
                    .bytes(None, preparation)?,
            };
            if std::str::from_utf8(&cur_bytes.value).ok().map(str::trim) != Some(source_generation)
            {
                return Err(managed_history::unavailable(
                    "captured branch CURRENT differs from its source generation",
                ));
            }
            push_src("dataset.json".into(), ds_bytes)?;
            push_src("CURRENT".into(), cur_bytes)?;
            if let Some(wal_input) = wal_interval_inputs.take() {
                let bdir = documents::Opened::root(broot, preparation)?;
                let gdir = bdir.child(source_generation, true, preparation)?;
                push_src(
                    format!("{source_generation}/commit.json"),
                    wal_input.commit_bytes,
                )?;
                push_src(
                    format!("{source_generation}/meta.json"),
                    gdir.child("meta.json", false, preparation)?
                        .bytes(None, preparation)?,
                )?;
                wal_interval_meta =
                    Some((wal_input.gen_dir, wal_input.base_seq, wal_input.wal_extent));
            }
        } else {
            let ds = managed_history::Bytes::from_serialized(2048, preparation, || {
                crate::commit::with_reader(
                    &crate::commit::dataset_file_bytes(
                        branch_id,
                        "memory",
                        crate::commit::now_ms(),
                    ),
                    managed_history::READER,
                )
            })?;
            push_src("dataset.json".into(), ds)?;
            let current_text = format!("{source_generation}\n");
            push_src(
                "CURRENT".into(),
                managed_history::Bytes::copy_from_slice(current_text.as_bytes(), preparation)?,
            )?;
        }
        let fdir = documents::Opened::root(froot, preparation)?;
        let src_managed = fdir
            .child(crate::managed::FILE, false, preparation)?
            .bytes(None, preparation)?;
        let src_history = primary::recorded_root_bytes(&src_managed.value, preparation)?
            .ok_or_else(|| {
                managed_history::unavailable("branch family has no published configuration root")
            })?;
        push_src(crate::managed::FILE.into(), src_managed)?;
        for (owner, bytes) in &installed.settings {
            push_src(
                owner.settings_file().into(),
                managed_history::Bytes::copy_from_slice(bytes, preparation)?,
            )?;
        }
        let lineage = src_history.lineage_id;
        let hist_dir = fdir.child("evidence-history", true, preparation)?.child(
            &lineage.to_string(),
            true,
            preparation,
        )?;
        let cov_bytes = hist_dir
            .child("coverage.json", false, preparation)?
            .bytes(None, preparation)?;
        let cov = managed_history::coverage_bytes(
            &cov_bytes.value,
            family_id,
            &src_history,
            preparation,
        )?;
        push_src(
            format!("evidence-history/{lineage}/coverage.json"),
            cov_bytes,
        )?;
        let gens_dir = hist_dir.child("generations", true, preparation)?;
        let mut cur_gen = src_history.identity(family_id);
        loop {
            control.check()?;
            preparation.check()?;
            let hex = cur_gen.generation.to_string();
            let hex = hex.strip_prefix("sha256:").expect("canonical digest");
            let file_name = format!("{hex}.json");
            let bytes = gens_dir
                .child(&file_name, false, preparation)?
                .bytes(None, preparation)?;
            let doc = managed_history::generation_bytes(&bytes.value, cur_gen, preparation)?;
            push_src(
                format!("evidence-history/{lineage}/generations/{file_name}"),
                bytes,
            )?;
            match doc.value.previous {
                Some(prev) => cur_gen.generation = prev,
                None => {
                    if cur_gen.generation != cov.value.activation_generation
                        || doc.value.ordinal != 1
                    {
                        return Err(managed_history::unavailable(
                            "family configuration lineage genesis differs",
                        ));
                    }
                    break;
                }
            }
        }
        drop(cov);
        // Include family's origin.json as a source member if present, propagating
        // any non-NotFound error (such as symlink refusal, budget, or cancellation).
        match fdir.child("evidence-history", true, preparation)?.child(
            "origin.json",
            false,
            preparation,
        ) {
            Ok(file) => {
                let origin_bytes = file.bytes(None, preparation)?;
                push_src(ROOT_FILE.into(), origin_bytes)?;
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        read_prior_imports_for_clone(froot, family, preparation)?
    } else {
        // In-memory branch.
        let ds = managed_history::Bytes::from_serialized(2048, preparation, || {
            crate::commit::with_reader(
                &crate::commit::dataset_file_bytes(branch_id, "memory", crate::commit::now_ms()),
                managed_history::READER,
            )
        })?;
        push_src("dataset.json".into(), ds)?;
        let current_text = format!("{source_generation}\n");
        push_src(
            "CURRENT".into(),
            managed_history::Bytes::copy_from_slice(current_text.as_bytes(), preparation)?,
        )?;
        let memory = installed.history_memory.as_ref().ok_or_else(|| {
            managed_history::unavailable("in-memory configuration history missing")
        })?;
        push_src(
            crate::managed::FILE.into(),
            managed_history::Bytes::from_serialized(
                managed_history::MAX_BYTES as u64,
                preparation,
                || Ok(installed.file_bytes()),
            )?,
        )?;
        for (owner, bytes) in &installed.settings {
            push_src(
                owner.settings_file().into(),
                managed_history::Bytes::copy_from_slice(bytes, preparation)?,
            )?;
        }
        let lineage = memory.coverage.value.lineage_id;
        let cov_bytes = managed_history::encode(&memory.coverage.value, preparation)?;
        push_src(
            format!("evidence-history/{lineage}/coverage.json"),
            cov_bytes,
        )?;
        let mut node = Some(memory.as_ref());
        while let Some(cur) = node {
            control.check()?;
            preparation.check()?;
            let hex = cur.prepared.identity.generation.to_string();
            let hex = hex.strip_prefix("sha256:").expect("canonical digest");
            push_src(
                format!("evidence-history/{lineage}/generations/{hex}.json"),
                managed_history::Bytes::copy_from_slice(&cur.prepared.bytes.value, preparation)?,
            )?;
            node = cur.previous.as_deref();
        }
        if let Some(mem_origin) = memory.origin.as_deref() {
            let (origin_bytes, prior_imports, prior_direct) = materialize_memory_origin_imports(
                family_id,
                installed,
                memory,
                mem_origin,
                preparation,
            )?;
            push_src(ROOT_FILE.into(), origin_bytes)?;
            (prior_imports, prior_direct)
        } else {
            (Vec::new(), Vec::new())
        }
    };

    // Optional WAL interval when capturing an unlinked branch directly.
    let mut intervals = Vec::new();
    let mut bases = Vec::new();
    if let (Some(broot), Some((gdir, base_seq, wal_extent))) = (branch_root, wal_interval_meta)
        && wal_extent > WAL_REC as u64
        && let Some(interval) = compute_wal_suffix_interval_bounded(
            broot,
            gdir,
            source_generation,
            branch_id,
            base_seq,
            0,
            Some(wal_extent),
            preparation,
        )?
    {
        intervals.push(interval);
    }

    // Build source descriptor and write imports closure into staging_dir.
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut staged_immutable: Vec<(String, Sha256Digest)> = Vec::new();
    for (prior_digest, files) in &prior_imports {
        all_sources.insert(*prior_digest.as_bytes());
        let hex = prior_digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        for (rel, bytes) in files {
            control.check()?;
            preparation.check()?;
            let path = format!("evidence-history/imports/{hex}/{rel}");
            let target = crate::store::definitions::stage_file(staging_dir, &path)?;
            crate::store::write_synced(&target, &bytes.value)?;
            let digest = Sha256Digest::of_bytes(&bytes.value, preparation.control())?;
            staged_immutable.push((path, digest));
        }
    }
    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &source_files {
        control.check()?;
        preparation.check()?;
        let pin = SourceFile {
            bytes: bytes.value.len() as u64,
            sha256: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
        };
        file_pins.insert(rel.clone(), pin);
    }
    let mut direct_sources = prior_direct;
    direct_sources.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    direct_sources.dedup();
    let descriptor = Descriptor {
        format: FORMAT,
        family_id,
        roots: BTreeMap::from([(
            "main".to_owned(),
            SourceRoot {
                logical_id: branch_id,
                head: head_seq,
                generation: source_generation.to_owned(),
            },
        )]),
        files: file_pins,
        sources: direct_sources,
        intervals,
    };
    descriptor.validate(preparation)?;
    let desc_bytes = managed_history::encode(&descriptor, preparation)?;
    let src_digest = Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())?;
    let src_hex = src_digest.to_string();
    let src_hex = src_hex.strip_prefix("sha256:").expect("canonical digest");
    for (rel, bytes) in &source_files {
        control.check()?;
        preparation.check()?;
        let path = format!("evidence-history/imports/{src_hex}/source/{rel}");
        let target = crate::store::definitions::stage_file(staging_dir, &path)?;
        crate::store::write_synced(&target, &bytes.value)?;
        staged_immutable.push((path, descriptor.files[rel].sha256));
    }
    let desc_rel = format!("evidence-history/imports/{src_hex}/origin.json");
    let desc_target = crate::store::definitions::stage_file(staging_dir, &desc_rel)?;
    crate::store::write_synced(&desc_target, &desc_bytes.value)?;
    staged_immutable.push((desc_rel, src_digest));
    all_sources.insert(*src_digest.as_bytes());
    // Authenticate the staged source descriptor identities before returning a public capture.
    let staged_node_dir = documents::Opened::root(
        &staging_dir.join(format!("evidence-history/imports/{src_hex}")),
        preparation,
    )?;
    documents::source_identities(&staged_node_dir, &descriptor, preparation)?;
    drop(staged_node_dir);

    // Establish fresh destination lineage for `branch_id` as a standalone dataset.
    let mut dest_installed = Installed::new(installed.graphs.clone(), installed.settings.clone())?;
    let genesis = managed_history::prepare(
        branch_id,
        Uuid::new_v4(),
        1,
        None,
        &dest_installed,
        preparation,
    )?;
    let first_own = head_seq
        .checked_add(1)
        .ok_or_else(|| managed_history::unavailable("branch backup own sequence overflows"))?;
    let coverage = managed_history::Coverage {
        format: managed_history::FORMAT,
        dataset_id: branch_id,
        lineage_id: genesis.identity.lineage_id,
        activation_generation: genesis.identity.generation,
        branches: BTreeMap::from([(
            branch_id,
            managed_history::BranchCoverage {
                activation_head: head_seq,
                first_bound_own: first_own,
            },
        )]),
    };
    managed_history::persist(staging_dir, &genesis, preparation)?;
    let coverage_digest = managed_history::persist_coverage(staging_dir, &coverage, preparation)?;
    let dest_lineage = genesis.identity.lineage_id;
    let gen_hex = genesis.identity.generation.to_string();
    let gen_hex = gen_hex.strip_prefix("sha256:").expect("canonical digest");
    staged_immutable.push((
        format!("evidence-history/{dest_lineage}/coverage.json"),
        coverage_digest,
    ));
    staged_immutable.push((
        format!("evidence-history/{dest_lineage}/generations/{gen_hex}.json"),
        genesis.identity.generation,
    ));
    dest_installed.configuration_history = Some(managed_history::Root {
        format: managed_history::FORMAT,
        lineage_id: dest_lineage,
        coverage: coverage_digest,
        current_generation: genesis.identity.generation,
    });

    let local = Location {
        branch: None,
        generation: dest_generation.to_owned(),
    };
    if wal_interval_meta.is_some() {
        bases.push(BaseImport {
            local: local.clone(),
            source: src_digest,
            original: Location {
                branch: None,
                generation: source_generation.to_owned(),
            },
        });
    }
    let ranges: Vec<RangeImport> = (0..descriptor.intervals.len())
        .map(|index| RangeImport {
            local: local.clone(),
            source: src_digest,
            interval: index as u32,
        })
        .collect();
    let origin = Origin {
        format: 2,
        dataset_id: branch_id,
        destination: Some(destination::Publication {
            genesis: genesis.identity,
            coverage: coverage_digest,
            creation_commit: None,
        }),
        sources: all_sources
            .into_iter()
            .map(Sha256Digest::from_digest_bytes)
            .collect(),
        bases,
        ranges,
    };
    origin.validate(preparation)?;
    let origin_bytes = managed_history::encode(&origin, preparation)?;
    let origin_digest = Sha256Digest::of_bytes(&origin_bytes.value, preparation.control())?;
    let origin_target = crate::store::definitions::stage_file(staging_dir, ROOT_FILE)?;
    crate::store::write_synced(&origin_target, &origin_bytes.value)?;
    staged_immutable.push((ROOT_FILE.to_owned(), origin_digest));

    let total_files = staged_immutable
        .len()
        .saturating_add(1)
        .saturating_add(dest_installed.settings.len());
    if total_files >= managed_history::capture::MAX_FILES {
        return Err(managed_history::unavailable(
            "configuration closure exceeds backup manifest limits",
        ));
    }
    let mut out = Vec::with_capacity(total_files);
    for (rel, digest) in staged_immutable {
        control.check()?;
        let file = managed_history::capture::pinned_file(
            &staging_dir.join(&rel),
            digest,
            preparation,
            control,
        )?;
        out.push(CapturedFile {
            path: rel,
            kind: FileKind::Immutable,
            len: file.metadata()?.len(),
            src: FileSource::File(file),
        });
    }
    let managed_bytes = dest_installed.file_bytes();
    let managed_path = staging_dir.join(crate::managed::FILE);
    crate::store::write_synced(&managed_path, &managed_bytes)?;
    let managed_file = managed_history::capture::pinned_file(
        &managed_path,
        Sha256Digest::of_bytes(&managed_bytes, preparation.control())?,
        preparation,
        control,
    )?;
    out.push(CapturedFile {
        path: crate::managed::FILE.into(),
        kind: FileKind::Meta,
        len: managed_file.metadata()?.len(),
        src: FileSource::File(managed_file),
    });
    for (owner, bytes) in &dest_installed.settings {
        control.check()?;
        let spath = staging_dir.join(owner.settings_file());
        crate::store::write_synced(&spath, bytes)?;
        let sfile = managed_history::capture::pinned_file(
            &spath,
            Sha256Digest::of_bytes(bytes, preparation.control())?,
            preparation,
            control,
        )?;
        out.push(CapturedFile {
            path: owner.settings_file().into(),
            kind: FileKind::Meta,
            len: sfile.metadata()?.len(),
            src: FileSource::File(sfile),
        });
    }
    Ok(out)
}

pub(in crate::store) type PriorImportFiles = (Sha256Digest, Vec<(String, managed_history::Bytes)>);
pub(in crate::store) type PriorImportsSnapshot = (Vec<PriorImportFiles>, Vec<Sha256Digest>);

/// Read a local dataset metadata file through an anchored `openat(O_NOFOLLOW)`
/// handle under `root`, accounting its bytes in `preparation`. When `optional`
/// is true, only `ErrorKind::NotFound` maps to `Ok(None)`.
pub(in crate::store) fn read_anchored_local_file(
    root: &Path,
    relative: &str,
    optional: bool,
    preparation: &Preparation,
) -> Result<Option<managed_history::Bytes>> {
    preparation.check()?;
    let opened = documents::Opened::root(root, preparation)?;
    match opened.relative(relative, false, preparation) {
        Ok(file) => file.bytes(None, preparation).map(Some),
        Err(Error::Io(e)) if optional && e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Build and write a clone's new destination configuration-history root, source
/// provenance closure (`evidence-history/imports/...`), and format 2
/// `evidence-history/origin.json` (with `creation_commit`) into `dir`.
///
/// Syncs every created/written intermediate directory under `evidence-history/`
/// deepest-first before publishing `evidence-history/origin.json`.
///
/// Returns the new destination genesis `ConfigurationIdentity` (to embed in
/// `gen-0001/commit.json`).
#[allow(clippy::too_many_arguments)]
pub(in crate::store) fn write_clone_destination(
    dir: &Path,
    clone_id: Uuid,
    root_commit: &CommitInfo,
    installed: &Installed,
    src_family: Uuid,
    src_logical: Uuid,
    src_head: u64,
    src_generation: &str,
    source_files: Vec<(String, managed_history::Bytes)>,
    prior_imports: &[PriorImportFiles],
    prior_direct_sources: Vec<Sha256Digest>,
    preparation: &Preparation,
) -> Result<ConfigurationIdentity> {
    write_clone_destination_with_sync(
        dir,
        clone_id,
        root_commit,
        installed,
        src_family,
        src_logical,
        src_head,
        src_generation,
        source_files,
        prior_imports,
        prior_direct_sources,
        preparation,
        &mut |path| crate::store::sync_dir(path),
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::store) fn write_clone_destination_with_sync(
    dir: &Path,
    clone_id: Uuid,
    root_commit: &CommitInfo,
    installed: &Installed,
    src_family: Uuid,
    src_logical: Uuid,
    src_head: u64,
    src_generation: &str,
    source_files: Vec<(String, managed_history::Bytes)>,
    prior_imports: &[PriorImportFiles],
    prior_direct_sources: Vec<Sha256Digest>,
    preparation: &Preparation,
    sync_directory: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<ConfigurationIdentity> {
    preparation.check()?;
    let mut synced_dirs: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    // 1. Copy any prior imported source directories into `dir/evidence-history/imports/`.
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (prior_digest, files) in prior_imports {
        all_sources.insert(*prior_digest.as_bytes());
        let hex = prior_digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        for (rel, bytes) in files {
            preparation.check()?;
            let path = format!("evidence-history/imports/{hex}/{rel}");
            write_nested_immutable(dir, &path, &bytes.value, preparation, &mut synced_dirs)?;
        }
    }

    // 2. Build the descriptor for this immediate source and write its closure.
    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &source_files {
        preparation.check()?;
        if !source_member(rel) {
            return Err(managed_history::unavailable(
                "clone source provenance contains an invalid member path",
            ));
        }
        let pin = SourceFile {
            bytes: bytes.value.len() as u64,
            sha256: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
        };
        if file_pins.insert(rel.clone(), pin).is_some() {
            return Err(managed_history::unavailable(
                "clone source provenance repeats a member",
            ));
        }
    }
    let mut direct_sources = prior_direct_sources;
    direct_sources.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    direct_sources.dedup();
    let descriptor = Descriptor {
        format: FORMAT,
        family_id: src_family,
        roots: BTreeMap::from([(
            "main".to_owned(),
            SourceRoot {
                logical_id: src_logical,
                head: src_head,
                generation: src_generation.to_owned(),
            },
        )]),
        files: file_pins,
        sources: direct_sources,
        intervals: Vec::new(),
    };
    descriptor.validate(preparation)?;
    let desc_bytes = managed_history::encode(&descriptor, preparation)?;
    let src_digest = Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())?;
    let src_hex = src_digest.to_string();
    let src_hex = src_hex.strip_prefix("sha256:").expect("canonical digest");
    for (rel, bytes) in &source_files {
        preparation.check()?;
        let path = format!("evidence-history/imports/{src_hex}/source/{rel}");
        write_nested_immutable(dir, &path, &bytes.value, preparation, &mut synced_dirs)?;
    }
    write_nested_immutable(
        dir,
        &format!("evidence-history/imports/{src_hex}/origin.json"),
        &desc_bytes.value,
        preparation,
        &mut synced_dirs,
    )?;
    all_sources.insert(*src_digest.as_bytes());
    // Sync every intermediate import directory deepest-first before publishing
    // `evidence-history/origin.json` and `CURRENT`.
    sync_directories_deepest_first(&synced_dirs, preparation, sync_directory)?;

    // 3. Establish the fresh destination lineage, genesis generation, and coverage.
    let mut dest_installed = Installed::new(installed.graphs.clone(), installed.settings.clone())?;
    let genesis = managed_history::prepare(
        clone_id,
        Uuid::new_v4(),
        1,
        None,
        &dest_installed,
        preparation,
    )?;
    let coverage = managed_history::Coverage {
        format: managed_history::FORMAT,
        dataset_id: clone_id,
        lineage_id: genesis.identity.lineage_id,
        activation_generation: genesis.identity.generation,
        branches: BTreeMap::from([(
            clone_id,
            managed_history::BranchCoverage {
                activation_head: 0,
                first_bound_own: 1,
            },
        )]),
    };
    managed_history::persist(dir, &genesis, preparation)?;
    let coverage_digest = managed_history::persist_coverage(dir, &coverage, preparation)?;
    dest_installed.configuration_history = Some(managed_history::Root {
        format: managed_history::FORMAT,
        lineage_id: genesis.identity.lineage_id,
        coverage: coverage_digest,
        current_generation: genesis.identity.generation,
    });
    for (owner, bytes) in &dest_installed.settings {
        preparation.check()?;
        crate::store::write_atomic(&dir.join(owner.settings_file()), bytes)?;
    }
    crate::store::write_atomic(
        &dir.join(crate::managed::FILE),
        &dest_installed.file_bytes(),
    )?;

    // 4. Write format 2 `evidence-history/origin.json` with `creation_commit` pin.
    let binding = ConfigurationBinding::Known {
        configuration: genesis.identity,
    };
    let creation_commit =
        destination::creation_digest(clone_id, root_commit, binding, preparation)?;
    let origin = Origin {
        format: 2,
        dataset_id: clone_id,
        destination: Some(destination::Publication {
            genesis: genesis.identity,
            coverage: coverage_digest,
            creation_commit: Some(creation_commit),
        }),
        sources: all_sources
            .into_iter()
            .map(Sha256Digest::from_digest_bytes)
            .collect(),
        bases: Vec::new(),
        ranges: Vec::new(),
    };
    origin.validate(preparation)?;
    let origin_bytes = managed_history::encode(&origin, preparation)?;
    crate::store::write_atomic(&dir.join(ROOT_FILE), &origin_bytes.value)?;
    sync_directory(&dir.join("evidence-history"))?;
    Ok(genesis.identity)
}

/// Reidentify a closed, already-validated, single-generation standalone restored
/// persistent directory whose configuration history is activated (used by
/// new-identity backup restores). Refuses directories with extra generations or
/// branch-table state (`branches.json` or `branches/`).
///
/// Preserves the source's immutable configuration history and small identity/base
/// documents under `evidence-history/imports/<source-digest>/`, preserves any
/// earlier imported origins and ranges without duplicating WAL files, establishes
/// a fresh destination configuration-history root and format 2
/// `evidence-history/origin.json`, and reidentifies `commit.json`, `commits.bin`,
/// `history.json`, `annotations.bin`, and `dataset.json` (raising `minimumReader`
/// to [`crate::commit::IMPORTED_DESTINATION_READER`]).
pub fn reidentify_activated_dir(
    root: &Path,
    new_id: Uuid,
    forked_from: ForkedFrom,
    control: &crate::task::Control,
) -> Result<()> {
    let preparation = managed_history::administrative_preparation_with(control.clone())?;
    reidentify_activated_dir_with(root, new_id, forked_from, &preparation)
}

pub(in crate::store) fn reidentify_activated_dir_with(
    root: &Path,
    new_id: Uuid,
    forked_from: ForkedFrom,
    preparation: &Preparation,
) -> Result<()> {
    preparation.check()?;
    if new_id.is_nil() {
        return Err(Error::invalid("new dataset identity is nil"));
    }
    // Enforce the standalone single-generation precondition before reading or
    // mutating any files: refuse branch state (`branches.json` or `branches/`).
    for unsupported in [
        crate::store::branching::BRANCHES_FILE,
        crate::store::branching::BRANCHES_DIR,
    ] {
        match std::fs::symlink_metadata(root.join(unsupported)) {
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "cannot reidentify directory containing branch state ({unsupported}); \
                     supply a standalone restored database directory"
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    // Read dataset identity and validate any existing origin through anchored,
    // budgeted decoders rather than unanchored path reads.
    let root_dir = documents::Opened::root(root, preparation)?;
    let old_dataset_bytes = root_dir
        .child("dataset.json", false, preparation)?
        .bytes(None, preparation)?;
    let (old_id, old_reader) =
        documents::dataset_id_and_reader(&old_dataset_bytes.value, preparation)?;
    if old_id != forked_from.id {
        return Err(managed_history::unavailable(
            "restored dataset identity differs from backup source",
        ));
    }
    let _ = opening_origin_info(root, preparation)?;
    if old_reader < managed_history::READER {
        return Err(Error::Unsupported(format!(
            "activated configuration history requires dataset reader {}; this dataset records reader {old_reader}",
            managed_history::READER
        )));
    }
    let current_bytes = root_dir
        .child("CURRENT", false, preparation)?
        .bytes(None, preparation)?;
    let gen_name = std::str::from_utf8(&current_bytes.value)
        .map_err(|_| managed_history::unavailable("CURRENT is not UTF-8"))?
        .trim()
        .to_owned();
    if !generation(&gen_name) {
        return Err(managed_history::unavailable(
            "CURRENT does not name a generation",
        ));
    }
    // Enforce the single-generation precondition: reidentification only handles
    // a single-generation restored directory, never one with older retained gens.
    for entry in std::fs::read_dir(root)? {
        preparation.check()?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name
            .strip_prefix("gen-")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            && name != gen_name
        {
            return Err(Error::Invalid(format!(
                "cannot reidentify directory containing non-current generation {name}"
            )));
        }
    }
    let gen_dir = root.join(&gen_name);

    // 1. Load the existing managed registry and any prior format 2 origin closure.
    let src_managed_bytes = root_dir
        .child(crate::managed::FILE, false, preparation)?
        .bytes(None, preparation)?;
    let src_history = primary::recorded_root_bytes(&src_managed_bytes.value, preparation)?
        .ok_or_else(|| {
            managed_history::unavailable("activated restore has no published configuration root")
        })?;
    let src_state = crate::store::managed::load_dir(root);
    let src_installed = match src_state {
        Some(crate::managed::ManagedState::Installed(installed))
            if installed.configuration_history.as_ref() == Some(&src_history) =>
        {
            installed
        }
        _ => {
            return Err(managed_history::unavailable(
                "activated restore registry cannot be enforced",
            ));
        }
    };
    let prior = DiskClosure::load(root, preparation)?;

    // 2. Collect all small source metadata members BEFORE mutating anything,
    //    retaining their `Preparation` reservations until dropped.
    let mut source_files: BTreeMap<String, managed_history::Bytes> = BTreeMap::new();
    let mut insert_source = |rel: String, bytes: managed_history::Bytes| -> Result<()> {
        if source_files.len() >= MAX_MEMBERS {
            return Err(managed_history::unavailable(
                "reidentified source closure exceeds member limit",
            ));
        }
        source_files.insert(rel, bytes);
        Ok(())
    };
    insert_source(
        "dataset.json".into(),
        managed_history::Bytes::copy_from_slice(&old_dataset_bytes.value, preparation)?,
    )?;
    insert_source(
        "family-dataset.json".into(),
        managed_history::Bytes::copy_from_slice(&old_dataset_bytes.value, preparation)?,
    )?;
    insert_source("CURRENT".into(), current_bytes)?;
    insert_source(crate::managed::FILE.into(), src_managed_bytes)?;
    for (owner, bytes) in &src_installed.settings {
        insert_source(
            owner.settings_file().into(),
            managed_history::Bytes::copy_from_slice(bytes, preparation)?,
        )?;
    }
    let anchored_gen = root_dir.child(&gen_name, true, preparation)?;
    let src_commit_bytes = anchored_gen
        .child("commit.json", false, preparation)?
        .bytes(None, preparation)?;
    let src_meta_bytes = anchored_gen
        .child("meta.json", false, preparation)?
        .bytes(None, preparation)?;
    drop(anchored_gen);
    insert_source(
        format!("{gen_name}/commit.json"),
        managed_history::Bytes::copy_from_slice(&src_commit_bytes.value, preparation)?,
    )?;
    insert_source(format!("{gen_name}/meta.json"), src_meta_bytes)?;
    for optional in ["history.json", "prefixes.json", "origin.json", ROOT_FILE] {
        match root_dir.relative(optional, false, preparation) {
            Ok(file) => {
                insert_source(optional.into(), file.bytes(None, preparation)?)?;
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    drop(root_dir);
    // Collect the source's complete `evidence-history/<src-lineage>` closure.
    let src_lineage = src_history.lineage_id;
    let src_lineage_dir = managed_history::directory(root, src_lineage)?;
    let src_cov_bytes =
        managed_history::read_bytes(&src_lineage_dir.join("coverage.json"), preparation)?;
    let src_cov =
        managed_history::coverage_bytes(&src_cov_bytes.value, old_id, &src_history, preparation)?;
    insert_source(
        format!("evidence-history/{src_lineage}/coverage.json"),
        src_cov_bytes,
    )?;
    let mut cur_gen = src_history.identity(old_id);
    loop {
        preparation.check()?;
        let hex = cur_gen.generation.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        let rel = format!("evidence-history/{src_lineage}/generations/{hex}.json");
        let bytes = managed_history::read_bytes(&root.join(&rel), preparation)?;
        let doc = managed_history::generation_bytes(&bytes.value, cur_gen, preparation)?;
        insert_source(rel, bytes)?;
        match doc.value.previous {
            Some(prev) => cur_gen.generation = prev,
            None => {
                if cur_gen.generation != src_cov.value.activation_generation
                    || doc.value.ordinal != 1
                {
                    return Err(managed_history::unavailable(
                        "source configuration lineage genesis differs",
                    ));
                }
                break;
            }
        }
    }
    drop(src_cov);

    // 3. Determine prior imported WAL ranges for `gen_name` and compute any new
    //    native suffix WAL interval in `<gen>/wal.log`.
    let local = Location {
        branch: None,
        generation: gen_name.clone(),
    };
    let mut preserved_ranges: Vec<RangeImport> = Vec::new();
    let mut prior_end: u64 = 0;
    if let Some(prior_closure) = &prior {
        for range in &prior_closure.origin.value.ranges {
            if range.local != local {
                continue;
            }
            let idx = prior_closure.source_index(range.source)?;
            let interval = prior_closure.nodes[idx]
                .descriptor
                .value
                .intervals
                .get(range.interval as usize)
                .ok_or_else(|| {
                    managed_history::unavailable("prior range has no source interval")
                })?;
            prior_end = prior_end.max(interval.end);
            preserved_ranges.push(RangeImport {
                local: range.local.clone(),
                source: range.source,
                interval: range.interval,
            });
        }
    }
    let physical_base_seq = {
        let allowance = preparation.room();
        let _loan = preparation.reserve(allowance)?;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        })?;
        crate::commit::decode_gen_commit_bytes_with(
            &gen_dir.join("commit.json"),
            &src_commit_bytes.value,
            &mut budget,
        )?
        .value
        .commit
        .seq
    };
    let new_interval = compute_wal_suffix_interval_bounded(
        root,
        &gen_dir,
        &gen_name,
        old_id,
        physical_base_seq,
        prior_end,
        None,
        preparation,
    )?;

    // 4. Build and write the source descriptor under `evidence-history/imports/<src_digest>/`.
    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &source_files {
        preparation.check()?;
        let pin = SourceFile {
            bytes: bytes.value.len() as u64,
            sha256: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
        };
        file_pins.insert(rel.clone(), pin);
    }
    let prior_direct: Vec<Sha256Digest> = prior
        .as_ref()
        .map(|c| c.origin.value.sources.clone())
        .unwrap_or_default();
    let intervals: Vec<SourceInterval> = new_interval.into_iter().collect();
    let descriptor = Descriptor {
        format: FORMAT,
        family_id: old_id,
        roots: BTreeMap::from([(
            "main".to_owned(),
            SourceRoot {
                logical_id: old_id,
                head: forked_from.seq,
                generation: gen_name.clone(),
            },
        )]),
        files: file_pins,
        sources: prior_direct,
        intervals,
    };
    descriptor.validate(preparation)?;
    let desc_bytes = managed_history::encode(&descriptor, preparation)?;
    let src_digest = Sha256Digest::of_bytes(&desc_bytes.value, preparation.control())?;
    let src_hex = src_digest.to_string();
    let src_hex = src_hex.strip_prefix("sha256:").expect("canonical digest");
    let mut synced_dirs: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for (rel, bytes) in &source_files {
        preparation.check()?;
        let path = format!("evidence-history/imports/{src_hex}/source/{rel}");
        write_nested_immutable(root, &path, &bytes.value, preparation, &mut synced_dirs)?;
    }
    write_nested_immutable(
        root,
        &format!("evidence-history/imports/{src_hex}/origin.json"),
        &desc_bytes.value,
        preparation,
        &mut synced_dirs,
    )?;
    sync_directories_deepest_first(&synced_dirs, preparation, &mut |p| {
        crate::store::sync_dir(p)
    })?;
    drop(source_files);
    drop(desc_bytes);

    // 5. Establish the new destination configuration-history lineage & coverage.
    let mut dest_installed =
        Installed::new(src_installed.graphs.clone(), src_installed.settings.clone())?;
    let genesis = managed_history::prepare(
        new_id,
        Uuid::new_v4(),
        1,
        None,
        &dest_installed,
        preparation,
    )?;
    let first_own = forked_from
        .seq
        .checked_add(1)
        .ok_or_else(|| managed_history::unavailable("destination own sequence overflows"))?;
    let coverage = managed_history::Coverage {
        format: managed_history::FORMAT,
        dataset_id: new_id,
        lineage_id: genesis.identity.lineage_id,
        activation_generation: genesis.identity.generation,
        branches: BTreeMap::from([(
            new_id,
            managed_history::BranchCoverage {
                activation_head: forked_from.seq,
                first_bound_own: first_own,
            },
        )]),
    };
    managed_history::persist(root, &genesis, preparation)?;
    let coverage_digest = managed_history::persist_coverage(root, &coverage, preparation)?;
    dest_installed.configuration_history = Some(managed_history::Root {
        format: managed_history::FORMAT,
        lineage_id: genesis.identity.lineage_id,
        coverage: coverage_digest,
        current_generation: genesis.identity.generation,
    });

    // 6. Build and write `evidence-history/origin.json` (format 2).
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    if let Some(prior_closure) = &prior {
        for s in &prior_closure.origin.value.sources {
            all_sources.insert(*s.as_bytes());
        }
    }
    all_sources.insert(*src_digest.as_bytes());
    let mut ranges = preserved_ranges;
    for index in 0..descriptor.intervals.len() {
        ranges.push(RangeImport {
            local: local.clone(),
            source: src_digest,
            interval: index as u32,
        });
    }
    ranges.sort_by(|a, b| {
        (&a.local, a.source.as_bytes(), a.interval).cmp(&(
            &b.local,
            b.source.as_bytes(),
            b.interval,
        ))
    });
    let origin = Origin {
        format: 2,
        dataset_id: new_id,
        destination: Some(destination::Publication {
            genesis: genesis.identity,
            coverage: coverage_digest,
            creation_commit: None,
        }),
        sources: all_sources
            .into_iter()
            .map(Sha256Digest::from_digest_bytes)
            .collect(),
        bases: vec![BaseImport {
            local: local.clone(),
            source: src_digest,
            original: local,
        }],
        ranges,
    };
    origin.validate(preparation)?;
    let origin_bytes = managed_history::encode(&origin, preparation)?;
    crate::store::write_atomic(&root.join(ROOT_FILE), &origin_bytes.value)?;
    crate::store::write_atomic(
        &root.join(crate::managed::FILE),
        &dest_installed.file_bytes(),
    )?;

    // 7. Rewrite `<gen>/commit.json`, `commits.bin`, `history.json`,
    //    `annotations.bin`, and `dataset.json` (last!).
    let new_commit_bytes = crate::commit::reidentified_generation_bytes(
        &src_commit_bytes.value,
        old_id,
        new_id,
        preparation,
    )?;
    crate::store::write_atomic(&gen_dir.join("commit.json"), &new_commit_bytes.value)?;
    reidentify_catalog_file(root, old_id, new_id, preparation)?;
    crate::history::reidentify_file(root, old_id, new_id)?;
    reidentify_annotations_file(root, old_id, new_id, preparation)?;
    let new_dataset_bytes = crate::commit::reidentified_dataset_bytes(
        &old_dataset_bytes.value,
        old_id,
        new_id,
        forked_from.seq,
        crate::commit::now_ms(),
        preparation,
    )?;
    crate::store::write_atomic(&root.join("dataset.json"), &new_dataset_bytes.value)?;

    // 8. Remove the superseded source lineage directory at top level (its
    //    verified immutable copy now lives under `evidence-history/imports/`).
    if src_lineage != genesis.identity.lineage_id {
        let _ =
            std::fs::remove_dir_all(root.join("evidence-history").join(src_lineage.to_string()));
    }
    crate::store::sync_dir(&gen_dir)?;
    crate::store::sync_dir(&root.join("evidence-history"))?;
    crate::store::sync_dir(root)?;
    Ok(())
}

#[cfg(test)]
pub(in crate::store) type WalHashChunkHook = Option<Box<dyn FnMut(usize)>>;

#[cfg(test)]
thread_local! {
    /// Test-only hook invoked after each 64 KiB chunk hashed in
    /// `compute_wal_suffix_interval_bounded`, allowing tests to trigger
    /// in-flight cancellation after WAL hashing has started.
    pub(in crate::store) static WAL_HASH_CHUNK_HOOK: std::cell::RefCell<WalHashChunkHook> =
        const { std::cell::RefCell::new(None) };
}

fn wal_hash_chunk_hook(_chunk_index: usize) {
    #[cfg(test)]
    WAL_HASH_CHUNK_HOOK.with(|hook| {
        if let Some(cb) = hook.borrow_mut().as_mut() {
            cb(_chunk_index);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn compute_wal_suffix_interval_bounded(
    root: &Path,
    gen_dir: &Path,
    gen_name: &str,
    old_id: Uuid,
    base_seq: u64,
    prior_end: u64,
    max_extent: Option<u64>,
    preparation: &Preparation,
) -> Result<Option<SourceInterval>> {
    let wal_path = gen_dir.join("wal.log");
    let mut file = match File::open(&wal_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file_len = match max_extent {
        Some(limit) => file.metadata()?.len().min(limit),
        None => file.metadata()?.len(),
    };
    if file_len == 0 {
        return Ok(None);
    }
    let allowance = preparation.room();
    let _loan = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let mut reader = std::io::BufReader::new(file.try_clone()?);
    let extent = wal::probe::committed_bound(&mut reader, file_len, &budget)?;
    let vocab = cold::head_vocabulary_controlled(gen_dir, root, &mut budget)?;
    let terms = vocab.value.len_with(&budget)?;
    let annotations = crate::annotations::seqs_controlled(root, old_id, &mut budget)?;
    let mut first_seq = None;
    let mut first_start = None;
    let mut last_seq = None;
    let mut last_end = prior_end;
    let proof = wal::probe::prove_prefix_with(
        &mut reader,
        extent,
        base_seq,
        u64::MAX,
        terms,
        annotations.value.as_deref(),
        &wal_path,
        &budget,
        &mut |obs| {
            if obs.point.offset > prior_end {
                if first_seq.is_none() {
                    first_seq = Some(obs.point.seq);
                    first_start = Some(obs.start_offset);
                }
                last_seq = Some(obs.point.seq);
                last_end = obs.point.offset;
            }
            Ok(())
        },
    )?;
    let (Some(first_seq), Some(first_start), Some(last_seq)) = (first_seq, first_start, last_seq)
    else {
        return Ok(None);
    };
    // Start at the first transaction record (`WAL_REC` when `prior_end == 0`, or
    // `prior_end` for later suffixes) so in-place v2 -> v3 WAL header upgrades
    // during jobs/control enablement do not invalidate the authenticated span.
    let start = if prior_end == 0 {
        first_start.max(WAL_REC as u64)
    } else {
        prior_end
    };
    let end = last_end.min(proof.head.offset);
    if start >= end {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(start))?;
    let mut hash = Sha256::new();
    let mut remaining = end - start;
    let mut buf = [0u8; 64 << 10];
    let mut chunk_index = 0usize;
    while remaining > 0 {
        preparation.check()?;
        let want = (remaining as usize).min(buf.len());
        file.read_exact(&mut buf[..want])?;
        hash.update(&buf[..want]);
        remaining -= want as u64;
        wal_hash_chunk_hook(chunk_index);
        chunk_index += 1;
    }
    preparation.check()?;
    Ok(Some(SourceInterval {
        root: "main".to_owned(),
        generation: gen_name.to_owned(),
        start,
        end,
        sha256: Sha256Digest::from_digest_bytes(hash.finalize().into()),
        first_seq,
        last_seq,
    }))
}

/// Stream-rewrite a fixed header at offset 0 of `path` using a constant 64 KiB
/// buffer charged to `preparation`, polling `preparation.check()` on every chunk,
/// then atomically replace `path`.
fn reidentify_streaming_header<const N: usize>(
    path: &Path,
    preparation: &Preparation,
    transform: impl FnOnce(&[u8; N]) -> Result<Option<[u8; N]>>,
) -> Result<()> {
    preparation.check()?;
    let mut src = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let len = src.metadata()?.len();
    if len < N as u64 {
        return Ok(());
    }
    let mut header = [0u8; N];
    src.read_exact(&mut header)?;
    let Some(updated) = transform(&header)? else {
        return Ok(());
    };
    let _buf = preparation.reserve(64 << 10)?;
    let tmp = path.with_extension(format!("reid-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut out = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        out.write_all(&updated)?;
        let mut chunk = [0u8; 64 << 10];
        loop {
            preparation.check()?;
            let n = src.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            out.write_all(&chunk[..n])?;
        }
        out.sync_all()?;
        drop(out);
        drop(src);
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            crate::store::sync_dir(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn reidentify_catalog_file(
    root: &Path,
    old_id: Uuid,
    new_id: Uuid,
    preparation: &Preparation,
) -> Result<()> {
    reidentify_streaming_header::<64>(&root.join("commits.bin"), preparation, |header| {
        crate::commit::reidentified_catalog_header(header, old_id, new_id).map(Some)
    })
}

fn reidentify_annotations_file(
    root: &Path,
    old_id: Uuid,
    new_id: Uuid,
    preparation: &Preparation,
) -> Result<()> {
    reidentify_streaming_header::<{ crate::annotations::REIDENTIFY_HEADER_BYTES }>(
        &root.join(crate::annotations::FILE),
        preparation,
        |header| crate::annotations::reidentified_header(header, old_id, new_id).map(Some),
    )
}

fn write_nested_immutable(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    preparation: &Preparation,
    synced_dirs: &mut BTreeSet<std::path::PathBuf>,
) -> Result<()> {
    preparation.check()?;
    let target = crate::store::definitions::stage_file(root, relative)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(&target) {
        Ok(mut file) => {
            for chunk in bytes.chunks(64 << 10) {
                preparation.check()?;
                file.write_all(chunk)?;
            }
            file.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = managed_history::read_bytes(&target, preparation)?;
            if existing.value != bytes {
                return Err(managed_history::unavailable(
                    "immutable imported provenance file already differs",
                ));
            }
        }
        Err(e) => return Err(e.into()),
    }
    let stop = root.join("evidence-history");
    let mut cur = target.parent();
    while let Some(dir) = cur.filter(|d| d.starts_with(&stop) && *d != stop) {
        synced_dirs.insert(dir.to_path_buf());
        cur = dir.parent();
    }
    Ok(())
}

fn sync_directories_deepest_first(
    dirs: &BTreeSet<std::path::PathBuf>,
    preparation: &Preparation,
    sync_directory: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    // Reverse order of canonical paths visits every child directory strictly
    // before its parent.
    for dir in dirs.iter().rev() {
        preparation.check()?;
        sync_directory(dir)?;
    }
    Ok(())
}

/// Collect the files of every prior `evidence-history/imports/<digest>/` directory
/// on disk under `root` through anchored `Opened` handles (for carrying forward
/// repeated identity changes during a clone), checking the live `family` origin
/// selection and retaining `Preparation` reservations.
pub(in crate::store) fn read_prior_imports_for_clone(
    root: &Path,
    family: &crate::store::family::Family,
    preparation: &Preparation,
) -> Result<PriorImportsSnapshot> {
    let Some(closure) = DiskClosure::load_for_family(root, family, preparation)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut prior_dirs = Vec::new();
    prior_dirs
        .try_reserve_exact(closure.nodes.len())
        .map_err(|_| preparation.exhausted())?;
    for node in &closure.nodes {
        preparation.check()?;
        let mut files = Vec::new();
        files
            .try_reserve_exact(node.descriptor.value.files.len() + 1)
            .map_err(|_| preparation.exhausted())?;
        let desc = node
            .directory
            .child("origin.json", false, preparation)?
            .bytes(None, preparation)?;
        if Sha256Digest::of_bytes(&desc.value, preparation.control())? != node.digest {
            return Err(managed_history::unavailable(
                "prior source descriptor changed during clone capture",
            ));
        }
        files.push(("origin.json".to_owned(), desc));
        for member in node.descriptor.value.files.keys() {
            preparation.check()?;
            let bytes = documents::read_member(
                &node.directory,
                &node.descriptor.value,
                member,
                preparation,
            )?;
            files.push((format!("source/{member}"), bytes));
        }
        prior_dirs.push((node.digest, files));
    }
    let direct = closure.origin.value.sources.clone();
    Ok((prior_dirs, direct))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_node(
        dir: &Path,
        digest: Sha256Digest,
        edges: Vec<Sha256Digest>,
        preparation: &Preparation,
    ) -> DiskNode {
        let family_id = Uuid::new_v4();
        let descriptor = Descriptor {
            format: FORMAT,
            family_id,
            roots: BTreeMap::from([(
                "main".to_owned(),
                SourceRoot {
                    logical_id: family_id,
                    head: 1,
                    generation: "gen-0001".to_owned(),
                },
            )]),
            files: BTreeMap::from([
                (
                    "CURRENT".to_owned(),
                    SourceFile {
                        bytes: 1,
                        sha256: digest,
                    },
                ),
                (
                    "dataset.json".to_owned(),
                    SourceFile {
                        bytes: 1,
                        sha256: digest,
                    },
                ),
                (
                    "family-dataset.json".to_owned(),
                    SourceFile {
                        bytes: 1,
                        sha256: digest,
                    },
                ),
                (
                    "managed.json".to_owned(),
                    SourceFile {
                        bytes: 1,
                        sha256: digest,
                    },
                ),
            ]),
            sources: edges,
            intervals: Vec::new(),
        };
        let encoded = managed_history::encode(&descriptor, preparation).unwrap();
        let owned = Descriptor::decode(&encoded.value, preparation).unwrap();
        let directory = documents::Opened::root(dir, preparation).unwrap();
        DiskNode {
            digest,
            descriptor: owned,
            directory,
        }
    }

    #[test]
    fn check_acyclic_rejects_self_loop_two_node_cycle_and_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let prep = Preparation::new(crate::task::Control::none(), 8 << 20).unwrap();
        let d_a = Sha256Digest::from_digest_bytes([0x11; 32]);
        let d_b = Sha256Digest::from_digest_bytes([0x22; 32]);

        // 1. Valid acyclic edge A -> B passes.
        let ok_nodes = vec![
            synthetic_node(tmp.path(), d_a, vec![d_b], &prep),
            synthetic_node(tmp.path(), d_b, vec![], &prep),
        ];
        check_acyclic(&[d_a, d_b], &ok_nodes, &prep).unwrap();

        // 2. Direct self-loop A -> A is refused as a cycle.
        let self_nodes = vec![synthetic_node(tmp.path(), d_a, vec![d_a], &prep)];
        let err = check_acyclic(&[d_a], &self_nodes, &prep).unwrap_err();
        assert!(
            err.to_string().contains("cycle"),
            "expected cycle refusal for self-loop, got {err}"
        );

        // 3. Two-node cycle A -> B -> A is refused as a cycle.
        let cycle_nodes = vec![
            synthetic_node(tmp.path(), d_a, vec![d_b], &prep),
            synthetic_node(tmp.path(), d_b, vec![d_a], &prep),
        ];
        let err = check_acyclic(&[d_a, d_b], &cycle_nodes, &prep).unwrap_err();
        assert!(
            err.to_string().contains("cycle"),
            "expected cycle refusal for 2-node cycle, got {err}"
        );

        // 4. Tight preparation refuses reservation cleanly.
        let tiny = Preparation::new(crate::task::Control::none(), 512).unwrap();
        assert!(matches!(
            check_acyclic(&[d_a, d_b], &ok_nodes, &tiny),
            Err(Error::BudgetExceeded(_))
        ));

        // 5. Cancelled preparation returns Error::Cancelled.
        let ctl = crate::task::Control::none();
        let cancelled = Preparation::new(ctl.clone(), 8 << 20).unwrap();
        ctl.cancel.cancel();
        assert!(matches!(
            check_acyclic(&[d_a, d_b], &ok_nodes, &cancelled),
            Err(Error::Cancelled)
        ));
    }
}
