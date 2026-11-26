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
    pub(super) origin: managed_history::Owned<Origin>,
    pub(super) nodes: Vec<DiskNode>,
    _held: Reservation,
}

pub(super) struct DiskNode {
    pub(super) digest: Sha256Digest,
    pub(super) descriptor: managed_history::Owned<Descriptor>,
    pub(super) directory: documents::Opened,
}

impl DiskClosure {
    /// Load and authenticate `evidence-history/origin.json` and every declared
    /// imported source under `evidence-history/imports/`. Returns `Ok(None)` if
    /// `origin.json` is absent and no orphan `imports/` directory exists.
    pub(in crate::store) fn load(root: &Path, preparation: &Preparation) -> Result<Option<Self>> {
        let Some((expected_digest, format)) = opening_origin_info(root, preparation)? else {
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
        preparation.check()?;
        Ok(Some(Self {
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
            let (orig_commit, orig_meta) =
                read_disk_original_base(node, &root_name, &base.original.generation, preparation)?;
            let actual_meta: crate::builder::IndexMeta = {
                let bytes = managed_history::read_bytes(&gen_dir.join("meta.json"), preparation)?;
                serde_json::from_slice(&bytes.value)
                    .map_err(|_| managed_history::unavailable("invalid copied meta.json"))?
            };
            let retagged = match orig_commit.configuration {
                ConfigurationBinding::Known { configuration } => {
                    ConfigurationBinding::ImportedUnbound {
                        source: configuration,
                    }
                }
                other => other,
            };
            if orig_meta != actual_meta
                || orig_commit.commit != actual.commit
                || orig_commit.origin != actual.origin
                || (actual.configuration != orig_commit.configuration
                    && actual.configuration != retagged)
            {
                return Err(managed_history::unavailable(
                    "imported original physical base differs from copied generation",
                ));
            }
            let mut header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &root_name,
                preparation,
            )?;
            match orig_commit.configuration {
                ConfigurationBinding::ImportedUnbound { source } => {
                    self.verify_prior_from(index, source, preparation)?;
                }
                binding => header.generation_using(
                    preparation,
                    orig_commit.commit.seq,
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
        } else {
            native.generation(
                family_root,
                preparation,
                actual.commit.seq,
                actual.configuration,
            )?;
            if matches!(actual.configuration, ConfigurationBinding::LegacyUnbound) {
                return Err(managed_history::unavailable(
                    "imported legacy base is missing its original physical assignment",
                ));
            }
        }
        Ok(native)
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
    /// suffix (`validation.rs`).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::store) fn prove_wal<R: Read + Seek>(
        &self,
        family_root: &Path,
        gen_dir: &Path,
        gen_name: &str,
        input: R,
        extent: u64,
        base: u64,
        terms: u64,
        annotated: Option<&[u64]>,
        wal_path: &Path,
        native: &mut primary::Header,
        budget: &mut ColdBudget<'_>,
        preparation: &Preparation,
    ) -> Result<()> {
        let local = Location {
            branch: None,
            generation: gen_name.to_owned(),
        };
        struct DiskRange {
            span: physical::Span,
            source: usize,
            header: primary::Header,
        }
        let mut ranges: Vec<DiskRange> = Vec::new();
        let actual_commit = crate::commit::read_gen_commit_full(gen_dir)?
            .ok_or_else(|| managed_history::unavailable("copied generation commit is missing"))?;
        let actual_meta: crate::builder::IndexMeta = {
            let bytes = managed_history::read_bytes(&gen_dir.join("meta.json"), preparation)?;
            serde_json::from_slice(&bytes.value)
                .map_err(|_| managed_history::unavailable("invalid copied meta.json"))?
        };
        for assignment in &self.origin.value.ranges {
            preparation.check()?;
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
                    managed_history::unavailable("imported range has no declared source interval")
                })?;
            let (orig_commit, orig_meta) =
                read_disk_original_base(node, &interval.root, &interval.generation, preparation)?;
            let retagged = match orig_commit.configuration {
                ConfigurationBinding::Known { configuration } => {
                    ConfigurationBinding::ImportedUnbound {
                        source: configuration,
                    }
                }
                other => other,
            };
            if orig_meta != actual_meta
                || orig_commit.commit != actual_commit.commit
                || orig_commit.origin != actual_commit.origin
                || (actual_commit.configuration != orig_commit.configuration
                    && actual_commit.configuration != retagged)
            {
                return Err(managed_history::unavailable(
                    "imported WAL range base differs from copied generation",
                ));
            }
            let header = documents::source_header(
                &node.directory,
                &node.descriptor.value,
                &interval.root,
                preparation,
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
        ranges.sort_unstable_by_key(|r| r.span.start);
        if ranges.windows(2).any(|p| p[0].span.end > p[1].span.start) {
            return Err(managed_history::unavailable(
                "ambiguous imported physical intervals",
            ));
        }
        let spans: Vec<physical::Span> = ranges
            .iter()
            .map(|r| physical::Span {
                start: r.span.start,
                end: r.span.end,
                sha256: r.span.sha256,
                first_seq: r.span.first_seq,
                last_seq: r.span.last_seq,
            })
            .collect();
        let mut config_error = None;
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
            &mut |observation| {
                if config_error.is_none() {
                    let matched = ranges.iter_mut().find(|r| {
                        r.span.start <= observation.start_offset
                            && observation.point.offset <= r.span.end
                    });
                    let result = if let Some(range) = matched {
                        let node = &self.nodes[range.source];
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
                        native.observation(family_root, preparation, observation)
                    };
                    if let Err(error) = result {
                        config_error = Some(error);
                    }
                }
                Ok(())
            },
        )?;
        if let Some(error) = config_error {
            return Err(error);
        }
        Ok(())
    }

    /// Resolve a WAL commit binding when `(None, gen_name)` has an imported range
    /// covering `end_offset` / `seq`. Returns `Ok(None)` if the commit lies outside
    /// every imported range (so the caller resolves it as a native commit).
    pub(in crate::store) fn resolve_wal_binding(
        &self,
        gen_name: &str,
        end_offset: u64,
        seq: u64,
        digest: Option<Sha256Digest>,
        preparation: &Preparation,
    ) -> Result<Option<ConfigurationBinding>> {
        let local = Location {
            branch: None,
            generation: gen_name.to_owned(),
        };
        for assignment in &self.origin.value.ranges {
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
                    managed_history::unavailable("imported range has no declared source interval")
                })?;
            if end_offset > interval.start
                && end_offset <= interval.end
                && seq >= interval.first_seq
                && seq <= interval.last_seq
            {
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
                return match digest {
                    Some(generation) => {
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
                        let header = documents::source_header(
                            &node.directory,
                            &node.descriptor.value,
                            &interval.root,
                            preparation,
                        )?;
                        if !header.is_pre_cutoff(seq) {
                            return Err(managed_history::unavailable(
                                "covered imported commit is missing its configuration reference",
                            ));
                        }
                        Ok(Some(ConfigurationBinding::LegacyUnbound))
                    }
                };
            }
        }
        Ok(None)
    }
}

fn read_disk_original_base(
    node: &DiskNode,
    root_name: &str,
    generation: &str,
    preparation: &Preparation,
) -> Result<(RetainedGenerationCommit, crate::builder::IndexMeta)> {
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
    Ok((physical.value, meta.value))
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
    let mut colors = vec![0u8; count];
    let mut stack: Vec<(usize, usize)> = Vec::new();
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

/// Resolve an imported WAL commit binding on a live store root, if the store has
/// a format 2 `evidence-history/origin.json` covering `(gen_name, end_offset, seq)`.
pub(in crate::store) fn resolve_imported_wal_binding(
    root: &Path,
    gen_name: &str,
    end_offset: u64,
    seq: u64,
    digest: Option<Sha256Digest>,
    preparation: &Preparation,
) -> Result<Option<ConfigurationBinding>> {
    let Some(closure) = DiskClosure::load(root, preparation)? else {
        return Ok(None);
    };
    closure.resolve_wal_binding(gen_name, end_offset, seq, digest, preparation)
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

/// Capture `evidence-history/origin.json` and every declared file under
/// `evidence-history/imports/` as FD-pinned, hash-verified `CapturedFile`s for
/// backup creation.
pub(in crate::store) fn capture_import_files(
    root: &Path,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    let Some(closure) = DiskClosure::load(root, preparation)? else {
        return Ok(Vec::new());
    };
    let origin_path = root.join(ROOT_FILE);
    let origin_bytes = managed_history::read_bytes(&origin_path, preparation)?;
    let origin_digest = Sha256Digest::of_bytes(&origin_bytes.value, preparation.control())?;
    drop(origin_bytes);
    let mut out = Vec::new();
    let origin_file =
        managed_history::capture::pinned_file(&origin_path, origin_digest, preparation, control)?;
    out.push(CapturedFile {
        path: ROOT_FILE.to_owned(),
        kind: FileKind::Immutable,
        len: origin_file.metadata()?.len(),
        src: FileSource::File(origin_file),
    });
    for node in &closure.nodes {
        control.check()?;
        let hex = node.digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        let desc_rel = format!("evidence-history/imports/{hex}/origin.json");
        let desc_file = managed_history::capture::pinned_file(
            &root.join(&desc_rel),
            node.digest,
            preparation,
            control,
        )?;
        out.push(CapturedFile {
            path: desc_rel,
            kind: FileKind::Immutable,
            len: desc_file.metadata()?.len(),
            src: FileSource::File(desc_file),
        });
        for (member_path, pin) in &node.descriptor.value.files {
            control.check()?;
            let rel = format!("evidence-history/imports/{hex}/source/{member_path}");
            let file = managed_history::capture::pinned_file(
                &root.join(&rel),
                pin.sha256,
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
    }
    Ok(out)
}

pub(in crate::store) type PriorImportFiles = (Sha256Digest, Vec<(String, Vec<u8>)>);
pub(in crate::store) type PriorImportsSnapshot = (Vec<PriorImportFiles>, Vec<Sha256Digest>);

/// Build and write a clone's new destination configuration-history root, source
/// provenance closure (`evidence-history/imports/...`), and format 2
/// `evidence-history/origin.json` (with `creation_commit`) into `dir`.
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
    source_files: Vec<(String, Vec<u8>)>,
    prior_imports: &[PriorImportFiles],
    prior_direct_sources: Vec<Sha256Digest>,
    preparation: &Preparation,
) -> Result<ConfigurationIdentity> {
    preparation.check()?;
    // 1. Copy any prior imported source directories into `dir/evidence-history/imports/`.
    let mut all_sources: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (prior_digest, files) in prior_imports {
        all_sources.insert(*prior_digest.as_bytes());
        let hex = prior_digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        for (rel, bytes) in files {
            preparation.check()?;
            let path = format!("evidence-history/imports/{hex}/{rel}");
            write_nested_immutable(dir, &path, bytes, preparation)?;
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
            bytes: bytes.len() as u64,
            sha256: Sha256Digest::of_bytes(bytes, preparation.control())?,
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
        write_nested_immutable(dir, &path, bytes, preparation)?;
    }
    write_nested_immutable(
        dir,
        &format!("evidence-history/imports/{src_hex}/origin.json"),
        &desc_bytes.value,
        preparation,
    )?;
    all_sources.insert(*src_digest.as_bytes());

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
    crate::store::sync_dir(&dir.join("evidence-history"))?;
    Ok(genesis.identity)
}

/// Reidentify a closed, already-validated persistent directory whose
/// configuration history is activated (used by new-identity backup restores).
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

fn reidentify_activated_dir_with(
    root: &Path,
    new_id: Uuid,
    forked_from: ForkedFrom,
    preparation: &Preparation,
) -> Result<()> {
    preparation.check()?;
    if new_id.is_nil() {
        return Err(Error::invalid("new dataset identity is nil"));
    }
    crate::formats::check_configuration_versions(root)?;
    let old_dataset_bytes = managed_history::read_bytes(&root.join("dataset.json"), preparation)?;
    let old_id = documents::dataset_id(&old_dataset_bytes.value, preparation)?;
    if old_id != forked_from.id {
        return Err(managed_history::unavailable(
            "restored dataset identity differs from backup source",
        ));
    }
    let current_bytes = managed_history::read_bytes(&root.join("CURRENT"), preparation)?;
    let gen_name = std::str::from_utf8(&current_bytes.value)
        .map_err(|_| managed_history::unavailable("CURRENT is not UTF-8"))?
        .trim()
        .to_owned();
    if !generation(&gen_name) {
        return Err(managed_history::unavailable(
            "CURRENT does not name a generation",
        ));
    }
    let gen_dir = root.join(&gen_name);

    // 1. Load the existing managed registry and any prior format 2 origin closure.
    let src_managed_bytes =
        managed_history::read_bytes(&root.join(crate::managed::FILE), preparation)?;
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

    // 2. Collect all small source metadata members BEFORE mutating anything.
    let mut source_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    source_files.insert("dataset.json".into(), old_dataset_bytes.value.clone());
    source_files.insert(
        "family-dataset.json".into(),
        old_dataset_bytes.value.clone(),
    );
    source_files.insert("CURRENT".into(), current_bytes.value.clone());
    source_files.insert(crate::managed::FILE.into(), src_managed_bytes.value.clone());
    for (owner, bytes) in &src_installed.settings {
        source_files.insert(owner.settings_file().into(), bytes.to_vec());
    }
    let src_commit_bytes = managed_history::read_bytes(&gen_dir.join("commit.json"), preparation)?;
    let src_meta_bytes = managed_history::read_bytes(&gen_dir.join("meta.json"), preparation)?;
    source_files.insert(
        format!("{gen_name}/commit.json"),
        src_commit_bytes.value.clone(),
    );
    source_files.insert(
        format!("{gen_name}/meta.json"),
        src_meta_bytes.value.clone(),
    );
    for optional in ["history.json", "prefixes.json", "origin.json", ROOT_FILE] {
        match managed_history::read_bytes(&root.join(optional), preparation) {
            Ok(bytes) => {
                source_files.insert(optional.into(), bytes.value);
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // Collect the source's complete `evidence-history/<src-lineage>` closure.
    let src_lineage = src_history.lineage_id;
    let src_lineage_dir = managed_history::directory(root, src_lineage)?;
    let src_cov_bytes =
        managed_history::read_bytes(&src_lineage_dir.join("coverage.json"), preparation)?;
    let src_cov =
        managed_history::coverage_bytes(&src_cov_bytes.value, old_id, &src_history, preparation)?;
    source_files.insert(
        format!("evidence-history/{src_lineage}/coverage.json"),
        src_cov_bytes.value,
    );
    let mut cur_gen = src_history.identity(old_id);
    loop {
        preparation.check()?;
        let hex = cur_gen.generation.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        let rel = format!("evidence-history/{src_lineage}/generations/{hex}.json");
        let bytes = managed_history::read_bytes(&root.join(&rel), preparation)?;
        let doc = managed_history::generation_bytes(&bytes.value, cur_gen, preparation)?;
        source_files.insert(rel, bytes.value);
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
    let physical_base = crate::commit::read_gen_commit_full(&gen_dir)?
        .ok_or_else(|| managed_history::unavailable("restored generation commit is missing"))?;
    let new_interval = compute_wal_suffix_interval(
        root,
        &gen_dir,
        &gen_name,
        old_id,
        physical_base.commit.seq,
        prior_end,
        preparation,
    )?;

    // 4. Build and write the source descriptor under `evidence-history/imports/<src_digest>/`.
    let mut file_pins = BTreeMap::new();
    for (rel, bytes) in &source_files {
        preparation.check()?;
        let pin = SourceFile {
            bytes: bytes.len() as u64,
            sha256: Sha256Digest::of_bytes(bytes, preparation.control())?,
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
    for (rel, bytes) in &source_files {
        preparation.check()?;
        let path = format!("evidence-history/imports/{src_hex}/source/{rel}");
        write_nested_immutable(root, &path, bytes, preparation)?;
    }
    write_nested_immutable(
        root,
        &format!("evidence-history/imports/{src_hex}/origin.json"),
        &desc_bytes.value,
        preparation,
    )?;

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
    reidentify_catalog_file(root, old_id, new_id)?;
    crate::history::reidentify_file(root, old_id, new_id)?;
    crate::annotations::reidentify(root, old_id, new_id)?;
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

fn compute_wal_suffix_interval(
    root: &Path,
    gen_dir: &Path,
    gen_name: &str,
    old_id: Uuid,
    base_seq: u64,
    prior_end: u64,
    preparation: &Preparation,
) -> Result<Option<SourceInterval>> {
    let wal_path = gen_dir.join("wal.log");
    let mut file = match File::open(&wal_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file_len = file.metadata()?.len();
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
    let terms = vocab.value.len_limited(&budget.owned_limit()?)?;
    let annotations = crate::annotations::seqs_controlled(root, old_id, &mut budget)?;
    let mut first_seq = None;
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
                }
                last_seq = Some(obs.point.seq);
                last_end = obs.point.offset;
            }
            Ok(())
        },
    )?;
    let (Some(first_seq), Some(last_seq)) = (first_seq, last_seq) else {
        return Ok(None);
    };
    let start = prior_end;
    let end = last_end.min(proof.head.offset);
    if start >= end {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(start))?;
    let mut hash = Sha256::new();
    let mut remaining = end - start;
    let mut buf = [0u8; 64 << 10];
    while remaining > 0 {
        let want = (remaining as usize).min(buf.len());
        file.read_exact(&mut buf[..want])?;
        hash.update(&buf[..want]);
        remaining -= want as u64;
    }
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

fn reidentify_catalog_file(root: &Path, old_id: Uuid, new_id: Uuid) -> Result<()> {
    let catalog = root.join("commits.bin");
    match std::fs::read(&catalog) {
        Ok(mut buf) if buf.len() >= 64 => {
            let header: [u8; 64] = buf[..64].try_into().expect("64-byte header");
            let updated = crate::commit::reidentified_catalog_header(&header, old_id, new_id)?;
            buf[..64].copy_from_slice(&updated);
            crate::store::write_atomic(&catalog, &buf)?;
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn write_nested_immutable(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    preparation: &Preparation,
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
            if let Some(parent) = target.parent() {
                crate::store::sync_dir(parent)?;
            }
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
    Ok(())
}

/// Collect the files of every prior `evidence-history/imports/<digest>/` directory
/// on disk under `root` (for carrying forward repeated identity changes during a
/// clone).
pub(in crate::store) fn read_prior_imports_for_clone(
    root: &Path,
    preparation: &Preparation,
) -> Result<PriorImportsSnapshot> {
    let Some(closure) = DiskClosure::load(root, preparation)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut prior_dirs = Vec::with_capacity(closure.nodes.len());
    for node in &closure.nodes {
        preparation.check()?;
        let hex = node.digest.to_string();
        let hex = hex.strip_prefix("sha256:").expect("canonical digest");
        let base = root.join("evidence-history/imports").join(hex);
        let mut files = Vec::new();
        let desc = managed_history::read_bytes(&base.join("origin.json"), preparation)?;
        if Sha256Digest::of_bytes(&desc.value, preparation.control())? != node.digest {
            return Err(managed_history::unavailable(
                "prior source descriptor changed during clone capture",
            ));
        }
        files.push(("origin.json".to_owned(), desc.value));
        for (member, pin) in &node.descriptor.value.files {
            preparation.check()?;
            let bytes =
                managed_history::read_bytes(&base.join("source").join(member), preparation)?;
            if bytes.value.len() as u64 != pin.bytes
                || Sha256Digest::of_bytes(&bytes.value, preparation.control())? != pin.sha256
            {
                return Err(managed_history::unavailable(
                    "prior source member changed during clone capture",
                ));
            }
            files.push((format!("source/{member}"), bytes.value));
        }
        prior_dirs.push((node.digest, files));
    }
    let direct = closure.origin.value.sources.clone();
    Ok((prior_dirs, direct))
}
