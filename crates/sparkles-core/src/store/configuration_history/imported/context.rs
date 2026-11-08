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
    let physical = crate::commit::decode_gen_commit_bytes_controlled(
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
pub(super) struct Pending {
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
    fold_legacy: bool,
    strict_from: Option<u64>,
    _held: Reservation,
}

/// The complete result keeps the real source payload before its final credit.
/// It is not Clone and has no constructor outside the completed physical proof.
pub(super) struct Completed {
    pub(super) replay: crate::store::Replay,
    pub(super) imports: Retained,
}

/// A future Generation-owned map has no Arc back to Generation or Family. Every
/// operation must still receive its actual physical owner and current authority.
pub(super) struct Retained {
    sources: Sources,
    ranges: Vec<Range>,
    lock: Arc<crate::store::ProcessLock>,
    root: std::path::PathBuf,
    generation_uid: u64,
    generation_name: String,
    base: CommitInfo,
    base_configuration: ConfigurationBinding,
    base_origin: Sha256Digest,
    fold_legacy: bool,
    strict_from: Option<u64>,
    _held: Reservation,
}

struct Headers {
    values: Vec<primary::Header>,
    _held: Reservation,
}

impl Retained {
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
            || (actual.origin == "baseline") != self.fold_legacy
            || actual.wal_strict_from != self.strict_from
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
    pub(super) fn opening(
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
        } else {
            native.generation(root, preparation, actual.commit.seq, actual.configuration)?;
            if matches!(actual.configuration, ConfigurationBinding::LegacyUnbound) {
                return Err(managed_history::unavailable(
                    "imported legacy base is missing its original physical assignment",
                ));
            }
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
            fold_legacy: actual.origin == "baseline",
            strict_from: actual.wal_strict_from,
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

    /// The caller lends the one immutable buffer already read by the genuine
    /// opening. Both proof and replay consume that exact slice. No path reopen,
    /// cursor assumption, data cache entry or truncation occurs in this method.
    pub(super) fn replay(
        mut self,
        from: &crate::store::ReplayFrom<'_>,
        bytes: &[u8],
        preparation: &Preparation,
    ) -> Result<Completed> {
        preparation.check()?;
        if !Arc::ptr_eq(from.generation, &self.generation)
            || from.base != self.base
            || from.fold_legacy != self.fold_legacy
            || from.strict_from != self.strict_from
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
            self.fold_legacy,
            self.strict_from,
            self.generation.dvocab.len(),
            from.annotated.map(|_| membership.as_slice()),
            from.path,
            &mut budget,
            &mut |observation| {
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
            fold_legacy: self.fold_legacy,
            strict_from: self.strict_from,
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
        || original.physical.wal_strict_from != actual.wal_strict_from
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
    pending.push(start);
    visited[start] = true;
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
