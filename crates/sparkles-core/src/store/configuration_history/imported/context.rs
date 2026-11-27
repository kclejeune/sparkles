//! Pending local physical context. Construction is tied to the genuine opening
//! and exact opened generation; no provisional observation grants replay rights.
use super::*;
use crate::access::Reservation;
use crate::store::cold::ColdBudget;
use std::sync::Arc;

struct OriginalBase {
    physical: crate::commit::RetainedGenerationCommit,
    meta: crate::builder::IndexMeta,
    _held: Reservation,
}

fn original_base(
    node: &closure::Node,
    root_name: &str,
    generation: &str,
    preparation: &Preparation,
) -> Result<OriginalBase> {
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
    let meta = documents::read_member(
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
    let meta: crate::store::cold::Loaded<crate::builder::IndexMeta> = budget.json(&meta.value)?;
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
    held.release_to(budget.used());
    preparation.check()?;
    Ok(OriginalBase {
        physical: physical.value,
        meta: meta.value,
        _held: held,
    })
}

fn root_name(location: &Location) -> String {
    location
        .branch
        .map_or_else(|| "main".to_owned(), |id| id.to_string())
}

fn retag(binding: ConfigurationBinding) -> ConfigurationBinding {
    match binding {
        ConfigurationBinding::Known { configuration } => ConfigurationBinding::ImportedUnbound {
            source: configuration,
        },
        other => other,
    }
}

struct Range {
    span: physical::Span,
    source: usize,
    header: primary::Header,
}

/// This remains pending until the whole same-byte prefix, interval digests and
/// real transaction/durability framing have finished. It exposes no observations.
pub(in crate::store) struct Pending {
    sources: Sources,
    ranges: Vec<Range>,
    native: primary::Header,
    generation: Arc<Generation>,
    family: Arc<crate::store::family::Family>,
    lock: Arc<crate::store::ProcessLock>,
    root: std::path::PathBuf,
    base: CommitInfo,
    base_configuration: ConfigurationBinding,
    base_origin: Sha256Digest,
    _held: Reservation,
}

/// The complete result keeps the real source payload before its final credit.
/// It is not Clone and has no constructor outside the completed physical proof.
pub(in crate::store) struct Completed {
    pub(in crate::store) replay: crate::store::Replay,
    pub(in crate::store) imports: Retained,
}

/// A future Generation-owned map has no Arc back to Generation or Family. Every
/// operation must still receive its actual physical owner and current authority.
pub(in crate::store) struct Retained {
    sources: Sources,
    ranges: Vec<Range>,
    lock: Arc<crate::store::ProcessLock>,
    root: std::path::PathBuf,
    generation_uid: u64,
    generation_name: String,
    base: CommitInfo,
    base_configuration: ConfigurationBinding,
    base_origin: Sha256Digest,
    _held: Reservation,
}

struct Headers {
    values: Vec<primary::Header>,
    _held: Reservation,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::store) struct RetainedWalBindingStage {
    pub(in crate::store) stage: &'static str,
    pub(in crate::store) used: u64,
    pub(in crate::store) room: u64,
    pub(in crate::store) limit: u64,
}

#[cfg(test)]
pub(in crate::store) type RetainedWalBindingHook = Option<Box<dyn FnMut(RetainedWalBindingStage)>>;

#[cfg(test)]
thread_local! {
    /// Test-only stage observer for `Retained::resolve_retained_wal_binding`.
    pub(in crate::store) static RETAINED_WAL_BINDING_HOOK: std::cell::RefCell<RetainedWalBindingHook> =
        const { std::cell::RefCell::new(None) };
}

fn retained_wal_binding_hook(_stage: &'static str, _preparation: &Preparation) {
    #[cfg(test)]
    RETAINED_WAL_BINDING_HOOK.with(|hook| {
        if let Some(cb) = hook.borrow_mut().as_mut() {
            cb(RetainedWalBindingStage {
                stage: _stage,
                used: _preparation.used(),
                room: _preparation.room(),
                limit: _preparation.limit(),
            });
        }
    });
}

impl Retained {
    /// Name of the opening generation whose imported ranges were proven at open.
    pub(in crate::store) fn generation_name(&self) -> &str {
        &self.generation_name
    }

    /// Return whether `origin.json` declares any imported WAL intervals for
    /// main generation `gen_name`.
    pub(in crate::store) fn has_ranges_for(&self, gen_name: &str) -> bool {
        self.sources
            .origin
            .value
            .ranges
            .iter()
            .any(|r| r.local.branch.is_none() && r.local.generation == gen_name)
    }

    /// Resolve a WAL commit binding on this opening's proven generation using the
    /// already-authenticated `Sources` and `ranges` without re-hashing the closure.
    /// Verifies that `origin.json` on disk has not been replaced since opening.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::store) fn resolve_wal_binding(
        &self,
        family: &crate::store::family::Family,
        gen_name: &str,
        start_offset: u64,
        end_offset: u64,
        seq: u64,
        digest: Option<Sha256Digest>,
        preparation: &Preparation,
    ) -> Result<Option<ConfigurationBinding>> {
        preparation.check()?;
        family.verify_origin_lock(&self.root, &self.lock)?;
        family.check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        if gen_name != self.generation_name {
            return Ok(None);
        }
        resolve_binding_in_ranges(
            &self.sources,
            &self.ranges,
            start_offset,
            end_offset,
            seq,
            digest,
            preparation,
        )
    }

    /// Prove a retained older main generation (`gen_name != self.generation_name`)
    /// through `seq` and its required complete imported intervals using the already
    /// authenticated `self.sources`, then resolve the WAL commit's binding strictly
    /// from the authoritative proof's observation for `seq`.
    /// Caller holds history exclusion so `gen_dir` stays alive during proof.
    pub(in crate::store) fn resolve_retained_wal_binding(
        &self,
        family: &crate::store::family::Family,
        gen_dir: &Path,
        gen_name: &str,
        actual: &crate::commit::RetainedGenerationCommit,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        preparation.check()?;
        retained_wal_binding_hook("entered", preparation);
        family.verify_origin_lock(&self.root, &self.lock)?;
        family.check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        if !self.has_ranges_for(gen_name) {
            return Err(managed_history::unavailable(
                "retained generation has no declared imported ranges",
            ));
        }
        let _paths = preparation.reserve(
            (gen_dir.as_os_str().len() as u64)
                .checked_add(gen_name.len() as u64)
                .and_then(|n| n.checked_mul(4))
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let local = Location {
            branch: None,
            generation: gen_name.to_owned(),
        };
        local.validate()?;
        let actual_meta =
            super::reidentify::read_meta_controlled(&gen_dir.join("meta.json"), preparation)?;
        retained_wal_binding_hook("setup_meta", preparation);
        let count = self
            .sources
            .origin
            .value
            .ranges
            .iter()
            .filter(|r| r.local == local)
            .count();
        let _storage = preparation.reserve(
            (count as u64)
                .checked_mul(
                    (std::mem::size_of::<Range>() + std::mem::size_of::<physical::Span>()) as u64,
                )
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(count)
            .map_err(|_| preparation.exhausted())?;
        for assignment in &self.sources.origin.value.ranges {
            preparation.check()?;
            if assignment.local != local {
                continue;
            }
            let index = source_index(&self.sources, assignment.source)?;
            let node = &self.sources.nodes[index];
            let interval = node
                .descriptor
                .value
                .intervals
                .get(assignment.interval as usize)
                .ok_or_else(|| {
                    managed_history::unavailable("imported range has no declared source interval")
                })?;
            let original = original_base(node, &interval.root, &interval.generation, preparation)?;
            retained_wal_binding_hook("setup_source", preparation);
            if original.meta != actual_meta.value
                || original.physical.commit != actual.commit
                || original.physical.origin != actual.origin
                || (actual.configuration != original.physical.configuration
                    && actual.configuration != retag(original.physical.configuration))
            {
                return Err(managed_history::unavailable(
                    "imported WAL range base differs from retained generation",
                ));
            }
            drop(original);
            let header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &interval.root,
                preparation,
            )?;
            ranges.push(Range {
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
        ranges.sort_unstable_by_key(|range| range.span.start);
        if ranges
            .windows(2)
            .any(|pair| pair[0].span.end > pair[1].span.start)
        {
            return Err(managed_history::unavailable(
                "ambiguous imported physical intervals",
            ));
        }
        // Scope proof to `seq` and every imported interval that starts at or before `seq`.
        ranges.retain(|range| range.span.first_seq <= seq);
        let mut needed = ranges;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(needed.len())
            .map_err(|_| preparation.exhausted())?;
        for range in &needed {
            spans.push(physical::Span {
                start: range.span.start,
                end: range.span.end,
                sha256: range.span.sha256,
                first_seq: range.span.first_seq,
                last_seq: range.span.last_seq,
            });
        }
        let proof_target = spans.last().map_or(seq, |s| seq.max(s.last_seq));
        let state = family.managed.load_full();
        let creation = self
            .sources
            .origin
            .value
            .destination
            .as_ref()
            .map(|dest| dest.creation_origin(actual, preparation))
            .transpose()?
            .flatten();
        let mut native = primary::Header::from_state(
            &self.root,
            actual.dataset_id,
            actual.dataset_id,
            creation,
            state.as_deref(),
            preparation,
        )?;
        retained_wal_binding_hook("before_proof", preparation);
        // Loan half of the remaining preparation room to the WAL parser's ColdBudget
        // while keeping the other half available for source publication callbacks.
        let allowance = preparation.room() / 2;
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
        let vocabulary = cold::head_vocabulary_controlled(gen_dir, &self.root, &mut budget)?;
        let terms = vocabulary.value.len_limited(&budget.owned_limit()?)?;
        let annotations =
            crate::annotations::seqs_controlled(&self.root, actual.dataset_id, &mut budget)?;
        let wal_path = gen_dir.join("wal.log");
        let file = std::fs::File::open(&wal_path)?;
        let extent = file.metadata()?.len();
        let mut config_error = None;
        let mut selected_obs = None;
        let _ = physical::prove_prefix_through(
            file,
            &spans,
            extent,
            actual.commit.seq,
            proof_target,
            terms,
            annotations.value.as_deref(),
            &wal_path,
            &mut budget,
            &mut |observation, _| {
                if config_error.is_none() {
                    let matched = needed.iter_mut().find(|r| {
                        (r.span.start <= observation.start_offset
                            || (r.span.start == 0 && observation.start_offset == WAL_REC as u64))
                            && observation.point.offset <= r.span.end
                    });
                    let result = if let Some(range) = matched {
                        let node = &self.sources.nodes[range.source];
                        range
                            .header
                            .observation_using(preparation, observation, &mut |identity| {
                                documents::source_publication(
                                    &node.directory,
                                    &node.descriptor.value,
                                    identity,
                                    preparation,
                                )
                                .map(|_| ())
                            })
                    } else {
                        native.observation(&self.root, preparation, observation)
                    };
                    if let Err(error) = result {
                        config_error = Some(error);
                    } else if observation.point.seq == seq {
                        if selected_obs.is_some() {
                            config_error = Some(managed_history::unavailable(
                                "retained WAL proof observed duplicate selected commit",
                            ));
                        } else {
                            selected_obs = Some(observation);
                        }
                    }
                }
                Ok(())
            },
        )?;
        if let Some(error) = config_error {
            return Err(error);
        }
        let selected = selected_obs.ok_or_else(|| {
            managed_history::unavailable("retained WAL proof has no selected binding")
        })?;
        drop(annotations);
        drop(vocabulary);
        let _ = budget;
        drop(_loan);
        family.verify_origin_lock(&self.root, &self.lock)?;
        family.check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        if let Some(imported) = resolve_binding_in_ranges(
            &self.sources,
            &needed,
            selected.start_offset,
            selected.point.offset,
            selected.point.seq,
            selected.configuration,
            preparation,
        )? {
            return Ok(imported);
        }
        match selected.configuration {
            Some(generation) => {
                let installed = match state.as_deref() {
                    Some(crate::managed::ManagedState::Installed(installed)) => Some(installed),
                    Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                        return Err(managed_history::unavailable(reason.clone()));
                    }
                    None => None,
                };
                let history = installed
                    .and_then(|i| i.configuration_history.as_ref())
                    .ok_or_else(|| {
                        managed_history::unavailable("WAL binding has no published lineage")
                    })?;
                Ok(managed_history::binding(ConfigurationIdentity {
                    dataset_id: actual.dataset_id,
                    lineage_id: history.lineage_id,
                    generation,
                }))
            }
            None => Ok(ConfigurationBinding::LegacyUnbound),
        }
    }

    /// Authenticate an `ImportedUnbound` identity against this opening's retained
    /// `Sources` after checking that `origin.json` on disk matches the opening.
    pub(in crate::store) fn verify_identity(
        &self,
        family: &crate::store::family::Family,
        identity: ConfigurationIdentity,
        preparation: &Preparation,
    ) -> Result<()> {
        preparation.check()?;
        family.verify_origin_lock(&self.root, &self.lock)?;
        family.check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        resolve_any_source(&self.sources, identity, preparation)
    }

    /// Verify live origin selection against the opening's pinned digest.
    pub(in crate::store) fn check_live_origin(
        &self,
        family: &crate::store::family::Family,
        preparation: &Preparation,
    ) -> Result<()> {
        preparation.check()?;
        family.verify_origin_lock(&self.root, &self.lock)?;
        family.check_origin_selection(opening_origin_selection(&self.root, preparation)?)
    }

    fn check_owner(
        &self,
        family: &crate::store::family::Family,
        generation: &Generation,
        actual: &crate::commit::RetainedGenerationCommit,
        preparation: &Preparation,
    ) -> Result<()> {
        preparation.check()?;
        let _paths = preparation.reserve(
            (self.root.as_os_str().len() as u64)
                .checked_mul(4)
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        family.verify_origin_lock(&self.root, &self.lock)?;
        if generation.uid != self.generation_uid
            || generation.name != self.generation_name
            || generation.dir.as_deref() != Some(self.root.join(&self.generation_name).as_path())
            || actual.commit != self.base
            || actual.configuration != self.base_configuration
            || Sha256Digest::of_bytes(actual.origin.as_bytes(), preparation.control())?
                != self.base_origin
            || actual.dataset_id != self.sources.origin.value.dataset_id
        {
            return Err(managed_history::unavailable(
                "retained import map is not this actual physical generation",
            ));
        }
        // Deliberately no equality to today's on-disk Origin digest: a genuine
        // future compaction may advance current selection while this old view's
        // actual generation/source owners remain live. Current grants and native
        // publication headers are separate per-operation inputs.
        preparation.check()
    }

    fn fresh_headers(&self, preparation: &Preparation) -> Result<Headers> {
        // Each Header is fixed-size. The result owns both Vec backing and its
        // real admission; no cached last-publication fact crosses operations.
        let bytes = (self.ranges.len() as u64)
            .checked_mul(std::mem::size_of::<primary::Header>() as u64)
            .and_then(|n| n.checked_add(256))
            .ok_or_else(|| preparation.exhausted())?;
        let held = preparation.reserve(bytes)?;
        let mut headers = Vec::new();
        headers
            .try_reserve_exact(self.ranges.len())
            .map_err(|_| preparation.exhausted())?;
        for range in &self.ranges {
            preparation.check()?;
            headers.push(range.header.fresh());
        }
        preparation.check()?;
        Ok(Headers {
            values: headers,
            _held: held,
        })
    }
}

impl Pending {
    /// Only the main opening path may use this entry. Linked/retained/capture
    /// actors must keep their own physical owner and are wired separately.
    pub(in crate::store) fn opening(
        root: &Path,
        family: &Arc<crate::store::family::Family>,
        lock: &Arc<crate::store::ProcessLock>,
        generation: &Arc<Generation>,
        actual: &crate::commit::RetainedGenerationCommit,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        let Some(sources) = Sources::opening(root, family, lock, preparation)? else {
            return Ok(None);
        };
        let destination = sources
            .origin
            .value
            .destination
            .as_ref()
            .filter(|_| sources.origin.value.format == 2)
            .ok_or_else(|| {
                Error::Unsupported(
                    "metadata-only origin1 cannot authenticate imported physical references".into(),
                )
            })?;
        destination.authenticate(root, family, lock, preparation)?;
        let creation = destination.creation_origin(actual, preparation)?;
        let count = sources.origin.value.ranges.len();
        let amount = (std::mem::size_of::<Self>() as u64)
            .checked_add(
                (count as u64)
                    .checked_mul((std::mem::size_of::<Range>() * 2) as u64)
                    .ok_or_else(|| preparation.exhausted())?,
            )
            .and_then(|n| n.checked_add((root.as_os_str().len() as u64).saturating_mul(8)))
            .and_then(|n| n.checked_add(8192))
            .ok_or_else(|| preparation.exhausted())?;
        let held = preparation.reserve(amount)?;
        if generation.link.is_some()
            || generation.dir.as_deref() != Some(root.join(&generation.name).as_path())
            || actual.dataset_id != sources.origin.value.dataset_id
        {
            return Err(Error::Unsupported(
                "imported opening needs its exact ordinary main physical owner".into(),
            ));
        }
        let state = family.managed.load_full();
        let mut native = primary::Header::from_state(
            root,
            actual.dataset_id,
            actual.dataset_id,
            creation,
            state.as_deref(),
            preparation,
        )?;
        let local = Location {
            branch: None,
            generation: generation.name.clone(),
        };
        local.validate()?;
        if let Some(base) = sources
            .origin
            .value
            .bases
            .iter()
            .find(|base| base.local == local)
        {
            let index = source_index(&sources, base.source)?;
            let node = &sources.nodes[index];
            let name = root_name(&base.original);
            let original = original_base(node, &name, &base.original.generation, preparation)?;
            same_base(&original, actual, generation)?;
            let mut header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &name,
                preparation,
            )?;
            match original.physical.configuration {
                ConfigurationBinding::ImportedUnbound { source } => {
                    resolve_prior(&sources, index, source, preparation)?;
                }
                binding => header.generation_using(
                    preparation,
                    original.physical.commit.seq,
                    binding,
                    &mut |identity| {
                        documents::source_publication(
                            &node.directory,
                            &node.descriptor.value,
                            identity,
                            preparation,
                        )
                        .map(|_| ())
                    },
                )?,
            }
            // A native binding never gains a source exemption. It must agree
            // with this destination's actual publication as it always did.
            if matches!(actual.configuration, ConfigurationBinding::Known { .. }) {
                native.generation(root, preparation, actual.commit.seq, actual.configuration)?;
            }
        } else if let ConfigurationBinding::ImportedUnbound { source } = actual.configuration {
            // A compaction of an imported destination before its first own commit
            // writes a fresh generation whose base sequence is still below the
            // destination's own cutoff and retains the authenticated source binding.
            if !native.is_pre_cutoff(actual.commit.seq) {
                return Err(managed_history::unavailable(
                    "covered destination generation cannot hold an imported configuration binding",
                ));
            }
            resolve_any_source(&sources, source, preparation)?;
        } else if matches!(actual.configuration, ConfigurationBinding::LegacyUnbound) {
            // A compaction of an imported destination whose source head was itself
            // pre-activation (LegacyUnbound) writes a fresh generation below cutoff.
            // Verify that it is not a clone creation root and that every declared
            // source covering this sequence genuinely left it LegacyUnbound.
            if creation.is_some() || !native.is_pre_cutoff(actual.commit.seq) {
                native.generation(root, preparation, actual.commit.seq, actual.configuration)?;
                return Err(managed_history::unavailable(
                    "imported legacy base is missing its original physical assignment",
                ));
            }
            verify_legacy_unbound_base(&sources, actual.commit.seq, preparation)?;
        } else {
            native.generation(root, preparation, actual.commit.seq, actual.configuration)?;
        }
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(count)
            .map_err(|_| preparation.exhausted())?;
        for assignment in &sources.origin.value.ranges {
            preparation.check()?;
            if assignment.local != local {
                continue;
            }
            let index = source_index(&sources, assignment.source)?;
            let node = &sources.nodes[index];
            let interval = node
                .descriptor
                .value
                .intervals
                .get(assignment.interval as usize)
                .ok_or_else(|| {
                    managed_history::unavailable("imported range has no declared source interval")
                })?;
            let original = original_base(node, &interval.root, &interval.generation, preparation)?;
            same_base(&original, actual, generation)?;
            let header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &interval.root,
                preparation,
            )?;
            ranges.push(Range {
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
        ranges.sort_unstable_by_key(|range| range.span.start);
        if ranges
            .windows(2)
            .any(|pair| pair[0].span.end > pair[1].span.start)
        {
            return Err(managed_history::unavailable(
                "ambiguous imported physical intervals",
            ));
        }
        family.verify_origin_lock(root, lock)?;
        preparation.check()?;
        Ok(Some(Self {
            sources,
            ranges,
            native,
            generation: generation.clone(),
            family: family.clone(),
            lock: lock.clone(),
            root: root.to_path_buf(),
            base: actual.commit,
            base_configuration: actual.configuration,
            base_origin: Sha256Digest::of_bytes(actual.origin.as_bytes(), preparation.control())?,
            _held: held,
        }))
    }

    fn observation(
        &mut self,
        observation: wal::probe::WalCommitObservation,
        preparation: &Preparation,
    ) -> Result<()> {
        let range = self.ranges.iter_mut().find(|range| {
            range.span.start <= observation.start_offset
                && observation.point.offset <= range.span.end
        });
        if let Some(range) = range {
            let node = &self.sources.nodes[range.source];
            range
                .header
                .observation_using(preparation, observation, &mut |identity| {
                    documents::source_publication(
                        &node.directory,
                        &node.descriptor.value,
                        identity,
                        preparation,
                    )
                    .map(|_| ())
                })
        } else {
            self.native
                .observation(&self.root, preparation, observation)
        }
    }

    /// Verify that an opening without a WAL file declares no imported WAL
    /// intervals for the opened generation, returning its retained import map.
    pub(in crate::store) fn verify_no_wal(self) -> Result<Retained> {
        if !self.ranges.is_empty() {
            return Err(managed_history::unavailable(
                "imported WAL is missing its declared source intervals",
            ));
        }
        Ok(Retained {
            generation_uid: self.generation.uid,
            generation_name: self.generation.name.clone(),
            sources: self.sources,
            ranges: self.ranges,
            lock: self.lock,
            root: self.root,
            base: self.base,
            base_configuration: self.base_configuration,
            base_origin: self.base_origin,
            _held: self._held,
        })
    }

    /// The caller lends the one immutable buffer already read by the genuine
    /// opening. Both proof and replay consume that exact slice. No path reopen,
    /// cursor assumption, data cache entry or truncation occurs in this method.
    pub(in crate::store) fn replay(
        mut self,
        from: &crate::store::ReplayFrom<'_>,
        bytes: &[u8],
        preparation: &Preparation,
    ) -> Result<Completed> {
        preparation.check()?;
        if !Arc::ptr_eq(from.generation, &self.generation)
            || from.base != self.base
            || from.path != self.root.join(&self.generation.name).join("wal.log")
        {
            return Err(managed_history::unavailable(
                "import replay is not the selected opening's actual physical generation",
            ));
        }
        self.family.verify_origin_lock(&self.root, &self.lock)?;
        let count = self.ranges.len();
        let members = from.annotated.map_or(0, |set| set.len());
        let _storage = preparation.reserve(
            (count as u64)
                .checked_mul(std::mem::size_of::<physical::Span>() as u64)
                .and_then(|n| {
                    n.checked_add((members as u64).checked_mul(std::mem::size_of::<u64>() as u64)?)
                })
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(count)
            .map_err(|_| preparation.exhausted())?;
        for range in &self.ranges {
            preparation.check()?;
            spans.push(physical::Span {
                start: range.span.start,
                end: range.span.end,
                sha256: range.span.sha256,
                first_seq: range.span.first_seq,
                last_seq: range.span.last_seq,
            });
        }
        let mut membership = Vec::new();
        membership
            .try_reserve_exact(members)
            .map_err(|_| preparation.exhausted())?;
        if let Some(set) = from.annotated {
            for seq in set {
                preparation.check()?;
                membership.push(*seq);
            }
        }
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        // The parser's buffer/fixed states have their own real loan. Original
        // publication callbacks use the remaining Preparation concurrently.
        let allowance = 128 << 10;
        let _parser = preparation.reserve(allowance)?;
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        })?;
        let mut publication_error = None;
        let (proof, _same_buffer) = physical::prove_prefix(
            std::io::Cursor::new(bytes),
            &spans,
            bytes.len() as u64,
            self.base.seq,
            u64::MAX,
            self.generation.dvocab.len(),
            from.annotated.map(|_| membership.as_slice()),
            from.path,
            &mut budget,
            &mut |observation, _| {
                if publication_error.is_none() {
                    publication_error = self.observation(observation, preparation).err();
                }
                Ok(())
            },
        )?;
        if let Some(error) = publication_error {
            return Err(error);
        }
        self.family
            .check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        self.family.verify_origin_lock(&self.root, &self.lock)?;
        preparation.check()?;
        let replay = crate::store::replay_wal(
            from,
            bytes,
            &mut |_, _| preparation.check(),
            &mut |observation| {
                if observation.point.offset > proof.head.offset {
                    return Err(managed_history::unavailable(
                        "replay exceeds the complete authenticated imported prefix",
                    ));
                }
                self.observation(observation, preparation)
            },
        )?;
        if replay.good as u64 != proof.head.offset
            || replay
                .commits
                .last()
                .map_or(self.base.seq, |commit| commit.seq)
                != proof.head.seq
        {
            return Err(managed_history::unavailable(
                "import replay differs from its complete same-byte prefix proof",
            ));
        }
        self.family
            .check_origin_selection(opening_origin_selection(&self.root, preparation)?)?;
        self.family.verify_origin_lock(&self.root, &self.lock)?;
        preparation.check()?;
        let retained = Retained {
            generation_uid: self.generation.uid,
            generation_name: self.generation.name.clone(),
            sources: self.sources,
            ranges: self.ranges,
            lock: self.lock,
            root: self.root,
            base: self.base,
            base_configuration: self.base_configuration,
            base_origin: self.base_origin,
            _held: self._held,
        };
        // Local opening actors die here. The transferred map owns no Arc back
        // into its eventual Generation/Family and grants no query by itself.
        Ok(Completed {
            replay,
            imports: retained,
        })
    }
}

fn resolve_binding_in_ranges(
    sources: &Sources,
    ranges: &[Range],
    start_offset: u64,
    end_offset: u64,
    seq: u64,
    digest: Option<Sha256Digest>,
    preparation: &Preparation,
) -> Result<Option<ConfigurationBinding>> {
    for range in ranges {
        let in_span = (range.span.start <= start_offset
            || (range.span.start == 0 && start_offset == WAL_REC as u64))
            && end_offset <= range.span.end
            && seq >= range.span.first_seq
            && seq <= range.span.last_seq;
        if !in_span {
            // Reject any partial/straddling overlap with a proven imported span.
            if start_offset < range.span.end && end_offset > range.span.start {
                return Err(managed_history::unavailable(
                    "WAL commit boundary straddles an imported interval",
                ));
            }
            continue;
        }
        let node = &sources.nodes[range.source];
        return match digest {
            Some(generation) => {
                let registry = documents::read_member(
                    &node.directory,
                    &node.descriptor.value,
                    crate::managed::FILE,
                    preparation,
                )?;
                let history = primary::recorded_root_bytes(&registry.value, preparation)?
                    .ok_or_else(|| {
                        managed_history::unavailable("imported source root is missing")
                    })?;
                drop(registry);
                let identity = ConfigurationIdentity {
                    dataset_id: node.descriptor.value.family_id,
                    lineage_id: history.lineage_id,
                    generation,
                };
                drop(documents::source_publication(
                    &node.directory,
                    &node.descriptor.value,
                    identity,
                    preparation,
                )?);
                Ok(Some(ConfigurationBinding::ImportedUnbound {
                    source: identity,
                }))
            }
            None => {
                if !range.header.is_pre_cutoff(seq) {
                    return Err(managed_history::unavailable(
                        "covered imported commit is missing its configuration reference",
                    ));
                }
                Ok(Some(ConfigurationBinding::LegacyUnbound))
            }
        };
    }
    Ok(None)
}

fn source_index(sources: &Sources, digest: Sha256Digest) -> Result<usize> {
    sources
        .nodes
        .binary_search_by(|node| node.digest.as_bytes().cmp(digest.as_bytes()))
        .map_err(|_| managed_history::unavailable("physical assignment source is missing"))
}

fn same_base(
    original: &OriginalBase,
    actual: &crate::commit::RetainedGenerationCommit,
    generation: &Generation,
) -> Result<()> {
    let binding = original.physical.configuration;
    if original.meta != generation.meta
        || original.physical.commit != actual.commit
        || original.physical.origin != actual.origin
        || (actual.configuration != binding && actual.configuration != retag(binding))
    {
        return Err(managed_history::unavailable(
            "imported original physical base differs from the actual opened generation",
        ));
    }
    Ok(())
}

/// A previously imported base retains its original published identity. Only
/// explicitly reachable source dependencies may supply that publication.
fn resolve_prior(
    sources: &Sources,
    start: usize,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<()> {
    let count = sources.nodes.len();
    let _held = preparation.reserve(
        (count as u64)
            .checked_mul((std::mem::size_of::<usize>() + 1) as u64)
            .and_then(|n| n.checked_add(1024))
            .ok_or_else(|| preparation.exhausted())?,
    )?;
    let mut visited = Vec::new();
    visited
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    visited.resize(count, false);
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    visited[start] = true;
    // ImportedUnbound refers only to a declared earlier-source dependency.
    // The starting source's own publication cannot establish that ancestry.
    for digest in &sources.nodes[start].descriptor.value.sources {
        preparation.check()?;
        let next = source_index(sources, *digest)?;
        if !visited[next] {
            visited[next] = true;
            pending.push(next);
        }
    }
    while let Some(index) = pending.pop() {
        preparation.check()?;
        let node = &sources.nodes[index];
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
            preparation.check()?;
            let next = source_index(sources, *digest)?;
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

/// Authenticate an `ImportedUnbound` identity against any declared source in
/// `sources` (used for compacted pre-cutoff bases and branches of an imported
/// destination).
pub(super) fn resolve_any_source(
    sources: &Sources,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<()> {
    for node in &sources.nodes {
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

/// Verify that a pre-cutoff compacted `LegacyUnbound` base at `seq` was genuinely
/// pre-activation in its original source lineage rather than a downgraded bound
/// commit or clone creation root.
fn verify_legacy_unbound_base(
    sources: &Sources,
    seq: u64,
    preparation: &Preparation,
) -> Result<()> {
    let mut covered_by_source = false;
    for node in &sources.nodes {
        preparation.check()?;
        let Some(main_root) = node.descriptor.value.roots.get("main") else {
            continue;
        };
        if main_root.head < seq {
            continue;
        }
        covered_by_source = true;
        let mut header =
            documents::source_header(&node.directory, &node.descriptor.value, "main", preparation)?;
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

#[cfg(test)]
impl Sources {
    /// Tests enter the private resolver through real captured source owners.
    pub(in crate::store) fn test_resolve_prior(
        &self,
        start: Sha256Digest,
        identity: ConfigurationIdentity,
        preparation: &Preparation,
    ) -> Result<()> {
        resolve_prior(self, source_index(self, start)?, identity, preparation)
    }
}
