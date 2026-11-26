//! Versioned canonical git serialization profile (`format_version = 1`).
//!
//! A canonical tree writes `.sparkles-git/manifest.json` alongside one file per non-empty
//! graph under `graphs/`, using either sorted, subject-grouped Turtle (`graphs/*.ttl`) or
//! sorted canonical N-Quads (`graphs/*.nq`). Blank nodes are canonicalized dataset-wide
//! with RDFC-1.0 (`SHA-256`) so isomorphic datasets produce byte-for-byte identical trees
//! across build configurations and cross-graph blank-node identity (including links inside
//! Sparkles' `cdt:List` and `cdt:Map` composite literals) round-trips losslessly.
//!
//! RDFC-1.0 canonicalization requires holding the dataset's quads in memory; writes and
//! reads therefore enforce explicit quad, blank-node, ambiguity, and file-size bounds via
//! [`WriteOptions`] and [`ReadOptions`].

use crate::Dataset;
use crate::commit::Receipt;
use crate::fsutil;
use crate::git::{GitError, GitResult};
use crate::store::Snapshot;
use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
use oxrdf::vocab::xsd;
use oxrdf::{
    BlankNode, Dataset as RdfDataset, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term,
    Triple,
};
use oxrdfio::{RdfFormat, RdfParser};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sparkles_core::jobs::Sha256Stream;
use sparkles_core::sparql::cdt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Supported format version of `.sparkles-git/manifest.json`.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;
/// Stable profile identifier recorded in `manifest.json`.
pub const MANIFEST_PROFILE_NAME: &str = "sparkles-git-v1";
/// Canonical blank-node labeling algorithm recorded in `manifest.json`.
pub const CANONICALIZATION_ALGORITHM: &str = "rdfc-1.0-sha256";
/// Directory holding git serialization metadata inside a canonical tree.
pub const METADATA_DIR: &str = ".sparkles-git";
/// Relative path to the manifest inside a canonical tree.
pub const MANIFEST_REL_PATH: &str = ".sparkles-git/manifest.json";
/// Directory holding per-graph RDF files inside a canonical tree.
pub const GRAPHS_DIR: &str = "graphs";

/// Default maximum number of quads admitted for in-memory RDFC-1.0 canonicalization and
/// canonical tree reads.
pub const DEFAULT_MAX_QUADS: usize = 20_000_000;
/// Default maximum number of distinct blank nodes admitted for canonicalization.
pub const DEFAULT_MAX_BLANK_NODES: usize = 1_000_000;
/// Default maximum number of indistinguishable adjacent blank nodes permitted in a single
/// RDFC-1.0 permutation group before canonicalization refuses to prevent factorial blowup.
pub const DEFAULT_MAX_AMBIGUOUS_GROUP: usize = 8;
/// Default maximum size of `.sparkles-git/manifest.json` in bytes (16 MiB).
pub const DEFAULT_MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
/// Default maximum size of a single per-graph RDF file in bytes (256 MiB).
pub const DEFAULT_MAX_GRAPH_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Default maximum aggregate bytes read across a canonical tree (1 GiB).
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

const RDF_TYPE_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

const RDFC10: CanonicalizationAlgorithm = CanonicalizationAlgorithm::Rdfc10 {
    hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
};

/// RDF syntax used for per-graph files in a canonical git tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CanonicalSyntax {
    /// Deterministic sorted, subject-grouped Turtle (`graphs/*.ttl`).
    #[default]
    Turtle,
    /// Deterministic sorted canonical N-Quads (`graphs/*.nq`).
    NQuads,
}

impl CanonicalSyntax {
    /// Lowercase name (`turtle` or `nquads`).
    pub fn as_str(self) -> &'static str {
        match self {
            CanonicalSyntax::Turtle => "turtle",
            CanonicalSyntax::NQuads => "nquads",
        }
    }

    /// File extension without leading dot (`ttl` or `nq`).
    pub fn extension(self) -> &'static str {
        match self {
            CanonicalSyntax::Turtle => "ttl",
            CanonicalSyntax::NQuads => "nq",
        }
    }

    /// Parse `turtle`/`ttl` or `nquads`/`nq` (ASCII case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "turtle" | "ttl" => Some(CanonicalSyntax::Turtle),
            "nquads" | "nq" => Some(CanonicalSyntax::NQuads),
            _ => None,
        }
    }
}

/// Kind of graph entry in [`CanonicalManifest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CanonicalGraphKind {
    Default,
    Named,
    Blank,
}

/// One graph file recorded in `.sparkles-git/manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalGraphEntry {
    pub kind: CanonicalGraphKind,
    /// Graph IRI when `kind == CanonicalGraphKind::Named`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iri: Option<String>,
    /// Canonical blank-node label (without `_:`) when `kind == CanonicalGraphKind::Blank`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Relative path under `graphs/` using forward slashes.
    pub path: String,
    /// Number of quads stored in this graph file.
    pub quads: u64,
    /// Lowercase hex SHA-256 of the serialized file bytes.
    pub sha256: String,
}

impl CanonicalGraphEntry {
    /// Convert this manifest descriptor to an `oxrdf::GraphName`, validating IRI/label syntax.
    pub fn graph_name(&self) -> GitResult<GraphName> {
        match self.kind {
            CanonicalGraphKind::Default => {
                if self.iri.is_some() || self.label.is_some() {
                    return Err(GitError::InvalidManifest(
                        "default graph entry must not set iri or label".into(),
                    ));
                }
                Ok(GraphName::DefaultGraph)
            }
            CanonicalGraphKind::Named => {
                if self.label.is_some() {
                    return Err(GitError::InvalidManifest(
                        "named graph entry must not set label".into(),
                    ));
                }
                let iri = self.iri.as_deref().ok_or_else(|| {
                    GitError::InvalidManifest("named graph entry is missing iri".into())
                })?;
                let node = NamedNode::new(iri).map_err(|e| {
                    GitError::InvalidManifest(format!("invalid named graph IRI {iri:?}: {e}"))
                })?;
                Ok(GraphName::NamedNode(node))
            }
            CanonicalGraphKind::Blank => {
                if self.iri.is_some() {
                    return Err(GitError::InvalidManifest(
                        "blank-node graph entry must not set iri".into(),
                    ));
                }
                let label = self.label.as_deref().ok_or_else(|| {
                    GitError::InvalidManifest("blank-node graph entry is missing label".into())
                })?;
                let bn = BlankNode::new(label).map_err(|e| {
                    GitError::InvalidManifest(format!(
                        "invalid blank-node graph label {label:?}: {e}"
                    ))
                })?;
                Ok(GraphName::BlankNode(bn))
            }
        }
    }
}

/// Versioned manifest stored at `.sparkles-git/manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalManifest {
    pub format_version: u32,
    pub profile: String,
    pub syntax: CanonicalSyntax,
    pub canonicalization: String,
    pub prefixes: BTreeMap<String, String>,
    pub graphs: Vec<CanonicalGraphEntry>,
}

/// Options for writing a canonical git tree.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// Output syntax for per-graph files (default: [`CanonicalSyntax::Turtle`]).
    pub syntax: CanonicalSyntax,
    /// Additional or overriding prefixes merged on top of the dataset's prefixes.
    pub extra_prefixes: BTreeMap<String, String>,
    /// Remove stale graph files that were recorded in a previous `.sparkles-git/manifest.json`
    /// and are absent from the new manifest (default: `true`). Unmanaged files are never
    /// deleted.
    pub prune_stale_graphs: bool,
    /// Maximum number of quads admitted into in-memory RDFC-1.0 canonicalization.
    pub max_quads: usize,
    /// Maximum number of distinct blank nodes admitted into canonicalization.
    pub max_blank_nodes: usize,
    /// Maximum number of indistinguishable adjacent blank nodes permitted in one
    /// RDFC-1.0 permutation bucket before refusing cleanly.
    pub max_canonicalize_ambiguous_group: usize,
    /// Maximum size of `.sparkles-git/manifest.json` in bytes.
    pub max_manifest_bytes: u64,
    /// Maximum size of a single per-graph RDF file in bytes.
    pub max_graph_file_bytes: u64,
    /// Maximum aggregate bytes written across a canonical tree.
    pub max_total_bytes: u64,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            syntax: CanonicalSyntax::Turtle,
            extra_prefixes: BTreeMap::new(),
            prune_stale_graphs: true,
            max_quads: DEFAULT_MAX_QUADS,
            max_blank_nodes: DEFAULT_MAX_BLANK_NODES,
            max_canonicalize_ambiguous_group: DEFAULT_MAX_AMBIGUOUS_GROUP,
            max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
            max_graph_file_bytes: DEFAULT_MAX_GRAPH_FILE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// Options for reading or verifying a canonical git tree.
#[derive(Clone, Debug)]
pub struct ReadOptions {
    /// Verify each graph file's SHA-256 digest and quad count against `manifest.json`
    /// (default: `true`).
    pub verify_checksums: bool,
    /// Enforce canonical profile layout invariants: non-empty graphs, canonical graph order
    /// and filenames, no duplicate quads within a graph file, and no unlisted files under
    /// `graphs/` (default: `true`).
    pub strict_layout: bool,
    /// Maximum size of `.sparkles-git/manifest.json` in bytes.
    pub max_manifest_bytes: u64,
    /// Maximum size of a single graph file in bytes.
    pub max_graph_file_bytes: u64,
    /// Maximum aggregate bytes read across all files in the tree.
    pub max_total_bytes: u64,
    /// Maximum total quads parsed across the tree.
    pub max_quads: u64,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            verify_checksums: true,
            strict_layout: true,
            max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
            max_graph_file_bytes: DEFAULT_MAX_GRAPH_FILE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_quads: DEFAULT_MAX_QUADS as u64,
        }
    }
}

/// Decoded contents of a canonical git serialization tree.
#[derive(Clone, Debug)]
pub struct CanonicalTree {
    pub manifest: CanonicalManifest,
    pub prefixes: BTreeMap<String, String>,
    pub quads: Vec<Quad>,
}

/// Summary returned by streaming verification ([`verify_git_tree`] / [`verify_git_tree_with`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub manifest: CanonicalManifest,
    pub graph_count: usize,
    pub quad_count: u64,
    pub prefix_count: usize,
}

/// Canonical ordering key for graphs: default graph first, then named IRIs, then blank-node
/// graphs by canonical label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GraphKey {
    Default,
    Named(String),
    Blank(String),
}

impl GraphKey {
    fn from_graph_name(g: &GraphName) -> Self {
        match g {
            GraphName::DefaultGraph => GraphKey::Default,
            GraphName::NamedNode(n) => GraphKey::Named(n.as_str().to_string()),
            GraphName::BlankNode(b) => GraphKey::Blank(b.as_str().to_string()),
        }
    }

    fn kind(&self) -> CanonicalGraphKind {
        match self {
            GraphKey::Default => CanonicalGraphKind::Default,
            GraphKey::Named(_) => CanonicalGraphKind::Named,
            GraphKey::Blank(_) => CanonicalGraphKind::Blank,
        }
    }
}

/// Write a Sparkles [`Snapshot`] and its prefixes as a versioned canonical git tree under `dir`.
pub fn write_snapshot_git_tree(
    snapshot: &Snapshot,
    prefixes: &BTreeMap<String, String>,
    dir: &Path,
    options: &WriteOptions,
) -> GitResult<CanonicalManifest> {
    write_snapshot_git_tree_tracked(snapshot, prefixes, dir, options).map(|(m, _)| m)
}

/// Internal helper that also returns the previously profile-owned graph paths pruned during
/// the write, so git export stages only profile-owned files and deletions.
pub(crate) fn write_snapshot_git_tree_tracked(
    snapshot: &Snapshot,
    prefixes: &BTreeMap<String, String>,
    dir: &Path,
    options: &WriteOptions,
) -> GitResult<(CanonicalManifest, Vec<String>)> {
    let mut quads = Vec::new();
    let mut overflow = false;
    snapshot.for_each_quad(|q| {
        if quads.len() >= options.max_quads {
            overflow = true;
            return Ok(());
        }
        if let Some(quad) = snapshot.quad_to_terms(q) {
            quads.push(quad);
        }
        Ok(())
    })?;
    if overflow {
        return Err(GitError::Format(format!(
            "dataset exceeds canonicalization quad limit ({})",
            options.max_quads
        )));
    }
    write_git_tree_tracked(quads, prefixes, dir, options)
}

/// Write an arbitrary set of RDF quads and prefixes as a versioned canonical git tree under `dir`.
pub fn write_git_tree(
    quads: impl IntoIterator<Item = Quad>,
    prefixes: &BTreeMap<String, String>,
    dir: &Path,
    options: &WriteOptions,
) -> GitResult<CanonicalManifest> {
    write_git_tree_tracked(quads, prefixes, dir, options).map(|(m, _)| m)
}

pub(crate) fn write_git_tree_tracked(
    quads: impl IntoIterator<Item = Quad>,
    prefixes: &BTreeMap<String, String>,
    dir: &Path,
    options: &WriteOptions,
) -> GitResult<(CanonicalManifest, Vec<String>)> {
    // 1. Read-only preflight of root, managed subdirectories, and any prior manifest BEFORE
    //    creating directories, canonicalizing, or mutating any files.
    let (clean_dir, dir_exists) = preflight_target_dir_no_symlink(dir)?;
    let prior_owned = if dir_exists {
        preflight_optional_child_dir_no_symlink(&clean_dir, METADATA_DIR)?;
        preflight_optional_child_dir_no_symlink(&clean_dir, GRAPHS_DIR)?;
        let manifest_path = resolve_safe_path(&clean_dir, MANIFEST_REL_PATH)?;
        read_prior_owned_graph_paths(&manifest_path, options.max_manifest_bytes)?
    } else {
        BTreeMap::new()
    };

    // 2. Bounded dataset-wide RDFC-1.0 canonicalization (preserving composite literal
    //    `cdt:List` / `cdt:Map` blank-node links).
    let rdf_dataset = canonicalize_dataset_bounded(
        quads,
        options.max_quads,
        options.max_blank_nodes,
        options.max_canonicalize_ambiguous_group,
    )?;

    // 3. Partition canonicalized quads deterministically by graph and immediately release
    //    the RdfDataset before formatting files.
    let mut by_graph: BTreeMap<GraphKey, Vec<Quad>> = BTreeMap::new();
    for q in rdf_dataset.iter() {
        let owned = q.into_owned();
        let key = GraphKey::from_graph_name(&owned.graph_name);
        by_graph.entry(key).or_default().push(owned);
    }
    drop(rdf_dataset);

    // 4. Build validated, sorted prefix table.
    let mut merged_prefixes = BTreeMap::new();
    for (k, v) in prefixes.iter().chain(options.extra_prefixes.iter()) {
        if is_valid_prefix_label(k) && oxiri::Iri::parse(v.as_str()).is_ok() {
            merged_prefixes.insert(k.clone(), v.clone());
        }
    }

    // 5. Assign collision-free relative filenames and preflight EVERY destination collision
    //    and stale prior-owned file BEFORE creating directories or mutating the tree.
    let mut used_paths: BTreeSet<String> = BTreeSet::new();
    let mut planned: Vec<(GraphKey, String, Vec<Quad>)> = Vec::with_capacity(by_graph.len());
    for (graph_key, graph_quads) in by_graph {
        let rel_path = assign_graph_path(&graph_key, options.syntax, &mut used_paths);
        validate_graph_path(&rel_path, options.syntax)?;
        planned.push((graph_key, rel_path, graph_quads));
    }

    let mut stale_to_prune: Vec<String> = Vec::new();
    if dir_exists {
        for (_, rel_path, _) in &planned {
            let full_path = resolve_safe_path(&clean_dir, rel_path)?;
            preflight_owned_graph_file(
                &full_path,
                rel_path,
                prior_owned.get(rel_path).map(String::as_str),
                options.max_graph_file_bytes,
                "overwrite",
            )?;
        }
        if options.prune_stale_graphs {
            for (rel, expected_sha) in &prior_owned {
                if used_paths.contains(rel) {
                    continue;
                }
                let full_path = resolve_safe_path(&clean_dir, rel)?;
                if preflight_owned_graph_file(
                    &full_path,
                    rel,
                    Some(expected_sha.as_str()),
                    options.max_graph_file_bytes,
                    "prune",
                )? {
                    stale_to_prune.push(rel.clone());
                }
            }
        }
    }

    // 6. Now create target and managed directories safely.
    let clean_dir = ensure_dir_no_symlink(&clean_dir)?;
    let meta_dir = ensure_child_dir_no_symlink(&clean_dir, METADATA_DIR)?;
    let graphs_dir = ensure_child_dir_no_symlink(&clean_dir, GRAPHS_DIR)?;
    let manifest_path = resolve_safe_path(&clean_dir, MANIFEST_REL_PATH)?;

    // 7. Stream each graph into an exclusively created temp file (`TempPath`, closing the
    //    descriptor immediately) while enforcing per-file and aggregate byte limits so an
    //    over-limit write fails atomically without leaving partial files behind.
    let mut total_bytes_written = 0u64;
    let mut staged_graphs: Vec<(tempfile::TempPath, PathBuf)> = Vec::with_capacity(planned.len());
    let mut entries = Vec::with_capacity(planned.len());

    for (graph_key, rel_path, graph_quads) in planned {
        let full_path = resolve_safe_path(&clean_dir, &rel_path)?;
        let quad_count = graph_quads.len() as u64;
        let remaining = options.max_total_bytes.saturating_sub(total_bytes_written);
        let file_cap = options.max_graph_file_bytes.min(remaining);

        let (temp_path, written, digest) =
            stage_atomic_stream_hashed(&full_path, &rel_path, file_cap, |w| {
                match options.syntax {
                    CanonicalSyntax::Turtle => {
                        write_canonical_turtle(&graph_quads, &merged_prefixes, w)
                    }
                    CanonicalSyntax::NQuads => write_canonical_nquads(&graph_quads, w),
                }
            })?;
        drop(graph_quads);
        total_bytes_written = total_bytes_written.saturating_add(written);
        staged_graphs.push((temp_path, full_path));

        let (iri, label) = match &graph_key {
            GraphKey::Default => (None, None),
            GraphKey::Named(s) => (Some(s.clone()), None),
            GraphKey::Blank(s) => (None, Some(s.clone())),
        };
        entries.push(CanonicalGraphEntry {
            kind: graph_key.kind(),
            iri,
            label,
            path: rel_path,
            quads: quad_count,
            sha256: digest,
        });
    }

    // 8. Serialize and size-check the manifest BEFORE publishing any staged graph file.
    let manifest = CanonicalManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        profile: MANIFEST_PROFILE_NAME.to_string(),
        syntax: options.syntax,
        canonicalization: CANONICALIZATION_ALGORITHM.to_string(),
        prefixes: merged_prefixes,
        graphs: entries,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| GitError::InvalidManifest(format!("failed to serialize manifest: {e}")))?;
    manifest_bytes.push(b'\n');
    let manifest_len = manifest_bytes.len() as u64;
    if manifest_len > options.max_manifest_bytes {
        return Err(GitError::Format(format!(
            "{MANIFEST_REL_PATH} ({manifest_len} bytes) exceeds maximum manifest size ({} bytes)",
            options.max_manifest_bytes
        )));
    }
    if total_bytes_written.saturating_add(manifest_len) > options.max_total_bytes {
        return Err(GitError::Format(format!(
            "canonical tree exceeds maximum aggregate size ({} bytes)",
            options.max_total_bytes
        )));
    }
    let (staged_manifest, _, _) = stage_atomic_stream_hashed(
        &manifest_path,
        MANIFEST_REL_PATH,
        options.max_manifest_bytes,
        |w| {
            w.write_all(&manifest_bytes)
                .map_err(|e| GitError::io(&manifest_path, e))
        },
    )?;

    // 9. Commit staged graph files, prune verified stale files, and publish manifest.
    let wrote_graphs = !staged_graphs.is_empty();
    for (temp_path, full_path) in staged_graphs {
        check_dest_file_not_symlink(&full_path)?;
        temp_path
            .persist(&full_path)
            .map_err(|e| GitError::io(&full_path, e.error))?;
    }

    let mut pruned_paths = Vec::new();
    if !stale_to_prune.is_empty() {
        verify_existing_dir_no_symlink(&graphs_dir, GRAPHS_DIR)?;
        for rel in stale_to_prune {
            let full = resolve_safe_path(&clean_dir, &rel)?;
            match fs::symlink_metadata(&full) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(GitError::UnsafePath {
                            path: rel,
                            reason: "symlink under graphs/ is not allowed".into(),
                        });
                    }
                    if meta.file_type().is_file() {
                        fs::remove_file(&full).map_err(|e| GitError::io(&full, e))?;
                        pruned_paths.push(rel);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(GitError::io(&full, e)),
            }
        }
    }
    if wrote_graphs || !pruned_paths.is_empty() {
        fsutil::sync_dir(&graphs_dir).map_err(|e| GitError::io(&graphs_dir, e))?;
    }

    verify_existing_dir_no_symlink(&meta_dir, METADATA_DIR)?;
    check_dest_file_not_symlink(&manifest_path)?;
    staged_manifest
        .persist(&manifest_path)
        .map_err(|e| GitError::io(&manifest_path, e.error))?;
    fsutil::sync_dir(&meta_dir).map_err(|e| GitError::io(&meta_dir, e))?;

    Ok((manifest, pruned_paths))
}

/// Read and strictly validate an existing `.sparkles-git/manifest.json` (if present),
/// returning its owned `rel_path -> sha256` map. Refuses unsupported format versions,
/// malformed JSON, non-canonical paths, and symlinks instead of silently overwriting them.
fn read_prior_owned_graph_paths(
    manifest_path: &Path,
    max_manifest_bytes: u64,
) -> GitResult<BTreeMap<String, String>> {
    match fs::symlink_metadata(manifest_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: MANIFEST_REL_PATH.to_string(),
                    reason: "existing manifest path must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: MANIFEST_REL_PATH.to_string(),
                    reason: "existing manifest path is not a regular file".into(),
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(GitError::io(manifest_path, e)),
    }
    let bytes = read_bounded_regular_file(manifest_path, MANIFEST_REL_PATH, max_manifest_bytes)?;
    let prior = parse_and_validate_manifest(&bytes, true)?;
    Ok(prior
        .graphs
        .into_iter()
        .map(|g| (g.path, g.sha256))
        .collect())
}

/// Preflight a graph path on disk before overwriting or pruning it.
///
/// Returns `Ok(true)` if the file exists and is verified to match `expected_sha`, `Ok(false)`
/// if it does not exist, or an `UnsafePath` error if the path is a symlink, non-regular
/// file, unmanaged (`expected_sha == None`), or modified relative to the prior manifest.
fn preflight_owned_graph_file(
    full_path: &Path,
    rel_path: &str,
    expected_sha: Option<&str>,
    max_graph_file_bytes: u64,
    verb: &str,
) -> GitResult<bool> {
    match fs::symlink_metadata(full_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(GitError::io(full_path, e)),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: rel_path.to_string(),
                    reason: "destination graph path must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: rel_path.to_string(),
                    reason: "destination graph path is not a regular file".into(),
                });
            }
        }
    }
    let Some(expected) = expected_sha else {
        return Err(GitError::UnsafePath {
            path: rel_path.to_string(),
            reason: format!("refusing to {verb} unmanaged file {rel_path:?}"),
        });
    };
    let (file, len) = open_regular_file_no_symlink(full_path, rel_path)?;
    if len > max_graph_file_bytes {
        return Err(GitError::UnsafePath {
            path: rel_path.to_string(),
            reason: format!(
                "existing file {rel_path:?} exceeds maximum graph size ({max_graph_file_bytes} bytes); \
                 refusing to {verb} without checksum verification"
            ),
        });
    }
    let mut reader = BoundedHashingReader::new(file, max_graph_file_bytes, rel_path.to_string());
    reader.drain_to_eof()?;
    let (_, actual_sha) = reader.finish();
    if actual_sha != expected {
        return Err(GitError::UnsafePath {
            path: rel_path.to_string(),
            reason: format!(
                "existing file {rel_path:?} does not match prior manifest checksum; \
                 refusing to {verb} modified or unmanaged file"
            ),
        });
    }
    Ok(true)
}

/// Read and strictly validate a canonical git serialization tree using default [`ReadOptions`].
pub fn read_git_tree(dir: &Path) -> GitResult<CanonicalTree> {
    read_git_tree_with(dir, &ReadOptions::default())
}

/// Read and strictly validate a canonical git serialization tree with explicit [`ReadOptions`].
pub fn read_git_tree_with(dir: &Path, options: &ReadOptions) -> GitResult<CanonicalTree> {
    let mut quads = Vec::new();
    let report = scan_git_tree_with(dir, options, |q| {
        quads.push(q);
        Ok(())
    })?;
    let prefixes = report.manifest.prefixes.clone();
    Ok(CanonicalTree {
        manifest: report.manifest,
        prefixes,
        quads,
    })
}

/// Verify a canonical git serialization tree by streaming graph files (without holding the
/// entire decoded dataset in memory), using default [`ReadOptions`].
pub fn verify_git_tree(dir: &Path) -> GitResult<VerifyReport> {
    verify_git_tree_with(dir, &ReadOptions::default())
}

/// Verify a canonical git serialization tree by streaming graph files with explicit
/// [`ReadOptions`].
///
/// Verification validates:
/// - non-symlink root directory (including directory aliases like trailing `/` or `/.`)
///   and non-symlink regular files (`O_NOFOLLOW` / `is_file`),
/// - duplicate-key-free JSON manifest, `format_version == 1`, profile & canonicalization IDs,
/// - canonical graph ordering, non-empty graph entries, canonical filenames under `graphs/`,
/// - absence of unlisted files or symlinks under `graphs/` (when `strict_layout == true`),
/// - canonical `c14n<N>` blank-node labels without leading zeros across graph names, quad
///   positions, nested triple terms, and composite literals (when `strict_layout == true`),
/// - bounded file sizes, total bytes, and total quad counts,
/// - streaming SHA-256 checksums and exact deduplicated quad counts (when `verify_checksums == true`),
/// - strict RDF syntax, graph membership, and duplicate-quad rejection per graph file.
///
/// Streaming verification does not re-run dataset-wide RDFC-1.0 canonicalization or
/// re-check serializer line-by-line byte ordering.
pub fn verify_git_tree_with(dir: &Path, options: &ReadOptions) -> GitResult<VerifyReport> {
    scan_git_tree_with(dir, options, |_| Ok(()))
}

fn scan_git_tree_with(
    dir: &Path,
    options: &ReadOptions,
    mut on_quad: impl FnMut(Quad) -> GitResult<()>,
) -> GitResult<VerifyReport> {
    let clean_dir = verify_existing_dir_no_symlink(dir, &dir.display().to_string())?;
    let manifest_path = resolve_safe_path(&clean_dir, MANIFEST_REL_PATH)?;
    let raw_bytes = read_bounded_regular_file(
        &manifest_path,
        MANIFEST_REL_PATH,
        options.max_manifest_bytes,
    )?;
    let mut total_bytes = raw_bytes.len() as u64;
    if total_bytes > options.max_total_bytes {
        return Err(GitError::InvalidManifest(format!(
            "tree exceeds maximum allowed read size ({} bytes)",
            options.max_total_bytes
        )));
    }
    let manifest = parse_and_validate_manifest(&raw_bytes, options.strict_layout)?;

    if options.strict_layout {
        check_no_unlisted_graph_files(&clean_dir, &manifest)?;
    }

    let mut total_quads = 0u64;
    for entry in &manifest.graphs {
        let expected_graph = entry.graph_name()?;
        let full_path = resolve_safe_path(&clean_dir, &entry.path)?;
        let (file, file_len) = open_regular_file_no_symlink(&full_path, &entry.path)?;
        if file_len > options.max_graph_file_bytes {
            return Err(GitError::InvalidManifest(format!(
                "{} exceeds maximum graph file size ({} bytes)",
                entry.path, options.max_graph_file_bytes
            )));
        }
        let remaining_total = options.max_total_bytes.saturating_sub(total_bytes);
        let read_cap = options.max_graph_file_bytes.min(remaining_total);
        if file_len > read_cap {
            return Err(GitError::InvalidManifest(format!(
                "tree exceeds maximum allowed read size ({} bytes)",
                options.max_total_bytes
            )));
        }

        let mut reader = BoundedHashingReader::new(file, read_cap, entry.path.clone());
        let mut graph_quad_count = 0u64;
        // Track per-graph quad uniqueness using 256-bit hashes of canonical quad strings
        // when strict_layout is enabled, keeping verification memory compact.
        let mut seen_hashes: Option<HashSet<[u8; 32]>> = options.strict_layout.then(HashSet::new);

        match manifest.syntax {
            CanonicalSyntax::Turtle => {
                let parser = RdfParser::from_format(RdfFormat::Turtle);
                for parsed in parser.for_reader(BufReader::new(&mut reader)) {
                    let q = parsed.map_err(|e| map_parse_or_limit_err(&entry.path, e))?;
                    let quad = Quad::new(q.subject, q.predicate, q.object, expected_graph.clone());
                    check_quad_limits_and_uniqueness(
                        &quad,
                        &entry.path,
                        &mut total_quads,
                        options.max_quads,
                        seen_hashes.as_mut(),
                    )?;
                    on_quad(quad)?;
                    graph_quad_count += 1;
                }
            }
            CanonicalSyntax::NQuads => {
                let parser = RdfParser::from_format(RdfFormat::NQuads);
                for parsed in parser.for_reader(BufReader::new(&mut reader)) {
                    let q = parsed.map_err(|e| map_parse_or_limit_err(&entry.path, e))?;
                    if q.graph_name != expected_graph {
                        return Err(GitError::RdfParse {
                            path: entry.path.clone(),
                            message: format!(
                                "quad graph {} does not match manifest graph {}",
                                q.graph_name, expected_graph
                            ),
                        });
                    }
                    check_quad_limits_and_uniqueness(
                        &q,
                        &entry.path,
                        &mut total_quads,
                        options.max_quads,
                        seen_hashes.as_mut(),
                    )?;
                    on_quad(q)?;
                    graph_quad_count += 1;
                }
            }
        }

        // Drain any trailing bytes (e.g. comments or trailing whitespace) so the SHA-256
        // covers the entire file.
        reader.drain_to_eof()?;
        let (bytes_read, actual_sha) = reader.finish();
        total_bytes = total_bytes.saturating_add(bytes_read);

        if options.verify_checksums {
            if actual_sha != entry.sha256 {
                return Err(GitError::InvalidManifest(format!(
                    "checksum mismatch for {}: expected {}, got {}",
                    entry.path, entry.sha256, actual_sha
                )));
            }
            if graph_quad_count != entry.quads {
                return Err(GitError::InvalidManifest(format!(
                    "quad count mismatch for {}: manifest declares {}, file parsed {}",
                    entry.path, entry.quads, graph_quad_count
                )));
            }
        } else if options.strict_layout && graph_quad_count == 0 {
            return Err(GitError::InvalidManifest(format!(
                "graph file {} contains zero quads",
                entry.path
            )));
        }
    }

    let graph_count = manifest.graphs.len();
    let prefix_count = manifest.prefixes.len();
    Ok(VerifyReport {
        manifest,
        graph_count,
        quad_count: total_quads,
        prefix_count,
    })
}

fn map_parse_or_limit_err(path: &str, err: oxrdfio::RdfParseError) -> GitError {
    if let oxrdfio::RdfParseError::Io(ref io_err) = err
        && io_err.kind() == io::ErrorKind::FileTooLarge
    {
        return GitError::InvalidManifest(io_err.to_string());
    }
    GitError::RdfParse {
        path: path.to_string(),
        message: err.to_string(),
    }
}

fn check_quad_limits_and_uniqueness(
    quad: &Quad,
    rel_path: &str,
    total_quads: &mut u64,
    max_quads: u64,
    seen_hashes: Option<&mut HashSet<[u8; 32]>>,
) -> GitResult<()> {
    *total_quads += 1;
    if *total_quads > max_quads {
        return Err(GitError::InvalidManifest(format!(
            "tree exceeds maximum quad limit ({max_quads})"
        )));
    }
    if let Some(seen) = seen_hashes {
        validate_quad_canonical_bnodes(quad, rel_path)?;
        let h = quad_hash_256(quad);
        if !seen.insert(h) {
            return Err(GitError::InvalidManifest(format!(
                "duplicate quad in canonical graph file {rel_path}"
            )));
        }
    }
    Ok(())
}

fn validate_quad_canonical_bnodes(quad: &Quad, rel_path: &str) -> GitResult<()> {
    if let NamedOrBlankNode::BlankNode(b) = &quad.subject
        && !is_canonical_bnode_label(b.as_str())
    {
        return Err(GitError::InvalidManifest(format!(
            "non-canonical blank-node label {:?} in {rel_path}; expected c14n<N>",
            b.as_str()
        )));
    }
    validate_term_canonical_bnodes(&quad.object, rel_path)?;
    if let GraphName::BlankNode(b) = &quad.graph_name
        && !is_canonical_bnode_label(b.as_str())
    {
        return Err(GitError::InvalidManifest(format!(
            "non-canonical blank-node graph label {:?} in {rel_path}; expected c14n<N>",
            b.as_str()
        )));
    }
    Ok(())
}

fn validate_term_canonical_bnodes(term: &Term, rel_path: &str) -> GitResult<()> {
    match term {
        Term::BlankNode(b) => {
            if !is_canonical_bnode_label(b.as_str()) {
                return Err(GitError::InvalidManifest(format!(
                    "non-canonical blank-node label {:?} in {rel_path}; expected c14n<N>",
                    b.as_str()
                )));
            }
            Ok(())
        }
        Term::Triple(tr) => {
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject
                && !is_canonical_bnode_label(b.as_str())
            {
                return Err(GitError::InvalidManifest(format!(
                    "non-canonical blank-node label {:?} in {rel_path}; expected c14n<N>",
                    b.as_str()
                )));
            }
            validate_term_canonical_bnodes(&tr.object, rel_path)
        }
        Term::Literal(l) if cdt::may_name_bnodes(l) => {
            let mut bad: Option<String> = None;
            let _ = cdt::relabel_literal(l, &mut |b| {
                if bad.is_none() && !is_canonical_bnode_label(b) {
                    bad = Some(b.to_string());
                }
                b.to_string()
            });
            if let Some(label) = bad {
                return Err(GitError::InvalidManifest(format!(
                    "non-canonical blank-node label {label:?} inside composite literal in {rel_path}; expected c14n<N>"
                )));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn quad_hash_256(q: &Quad) -> [u8; 32] {
    let mut s = Sha256Stream::new();
    s.update(q.to_string().as_bytes());
    let hex = s.finalize_hex();
    let mut out = [0u8; 32];
    for (i, &[hi_b, lo_b]) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hi = (hi_b as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (lo_b as char).to_digit(16).unwrap_or(0) as u8;
        out[i] = (hi << 4) | lo;
    }
    out
}

fn check_no_unlisted_graph_files(dir: &Path, manifest: &CanonicalManifest) -> GitResult<()> {
    let graphs_dir = dir.join(GRAPHS_DIR);
    match fs::symlink_metadata(&graphs_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: GRAPHS_DIR.to_string(),
                    reason: "graphs directory must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: GRAPHS_DIR.to_string(),
                    reason: "graphs path is not a directory".into(),
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if manifest.graphs.is_empty() {
                return Ok(());
            }
            return Err(GitError::InvalidManifest(
                "manifest lists graphs but graphs/ directory is missing".into(),
            ));
        }
        Err(e) => return Err(GitError::io(&graphs_dir, e)),
    }

    let expected: HashSet<&str> = manifest.graphs.iter().map(|g| g.path.as_str()).collect();
    for entry in fs::read_dir(&graphs_dir).map_err(|e| GitError::io(&graphs_dir, e))? {
        let entry = entry.map_err(|e| GitError::io(&graphs_dir, e))?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            return Err(GitError::UnsafePath {
                path: format!("{GRAPHS_DIR}/{}", name.to_string_lossy()),
                reason: "non-UTF-8 filename in graphs/".into(),
            });
        };
        let rel = format!("{GRAPHS_DIR}/{name_str}");
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| GitError::io(entry.path(), e))?;
        if meta.file_type().is_symlink() {
            return Err(GitError::UnsafePath {
                path: rel,
                reason: "symlink inside graphs/ is not allowed".into(),
            });
        }
        if !expected.contains(rel.as_str()) {
            return Err(GitError::InvalidManifest(format!(
                "unlisted entry {rel:?} present in {GRAPHS_DIR}/"
            )));
        }
    }
    Ok(())
}

/// Read a canonical git tree from `dir` and import its quads and prefixes into `dataset`
/// in one additive write transaction.
pub fn import_git_tree(dataset: &Dataset, dir: &Path) -> GitResult<Receipt> {
    import_git_tree_with(
        dataset,
        dir,
        &ReadOptions::default(),
        crate::guard::WriteOptions::default(),
    )
}

/// Read a canonical git tree from `dir` and import it into `dataset` with explicit options.
///
/// Quads and prefixes are published jointly in one guarded write transaction: dry runs,
/// refused writes, and prefix persistence errors abort before quads become visible.
pub fn import_git_tree_with(
    dataset: &Dataset,
    dir: &Path,
    read_options: &ReadOptions,
    write_options: crate::guard::WriteOptions,
) -> GitResult<Receipt> {
    let tree = read_git_tree_with(dir, read_options)?;
    let (_, receipt) = dataset.transaction_receipt_with(write_options, |tx| {
        for q in &tree.quads {
            tx.insert(q.as_ref())?;
        }
        if !tree.prefixes.is_empty() {
            tx.stage_prefixes(tree.prefixes.clone());
        }
        Ok(())
    })?;
    Ok(receipt)
}

// ------------------------------------------------------------ validation & paths ----

/// Reject any duplicate object keys in JSON input before value/schema parsing.
pub(crate) fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), String> {
    struct NoDupSeed;

    impl<'de> DeserializeSeed<'de> for NoDupSeed {
        type Value = ();

        fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
            deserializer.deserialize_any(NoDupVisitor)
        }
    }

    struct NoDupVisitor;

    impl<'de> Visitor<'de> for NoDupVisitor {
        type Value = ();

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("any valid JSON value without duplicate object keys")
        }

        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while seq.next_element_seed(NoDupSeed)?.is_some() {}
            Ok(())
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut seen = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate JSON key {key:?}"
                    )));
                }
                map.next_value_seed(NoDupSeed)?;
            }
            Ok(())
        }
    }

    let mut de = serde_json::Deserializer::from_slice(bytes);
    NoDupSeed.deserialize(&mut de).map_err(|e| e.to_string())?;
    de.end().map_err(|e| e.to_string())
}

/// Parse raw manifest JSON bytes, checking duplicate keys and `format_version` BEFORE typed
/// deserialization so future versions always yield [`GitError::UnsupportedFormatVersion`].
pub(crate) fn parse_and_validate_manifest(
    bytes: &[u8],
    strict_layout: bool,
) -> GitResult<CanonicalManifest> {
    reject_duplicate_json_keys(bytes)
        .map_err(|e| GitError::InvalidManifest(format!("malformed JSON: {e}")))?;

    let raw: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| GitError::InvalidManifest(format!("malformed JSON: {e}")))?;
    let obj = raw
        .as_object()
        .ok_or_else(|| GitError::InvalidManifest("manifest root must be a JSON object".into()))?;
    let version_val = obj
        .get("format_version")
        .ok_or_else(|| GitError::InvalidManifest("missing required field format_version".into()))?;
    let found_version_u64 = version_val.as_u64().ok_or_else(|| {
        GitError::InvalidManifest("format_version must be a non-negative integer".into())
    })?;
    let found_version = u32::try_from(found_version_u64).unwrap_or(u32::MAX);
    if found_version != MANIFEST_FORMAT_VERSION {
        return Err(GitError::UnsupportedFormatVersion {
            file: MANIFEST_REL_PATH.to_string(),
            found: found_version,
            supported: MANIFEST_FORMAT_VERSION,
        });
    }

    let manifest: CanonicalManifest = serde_json::from_value(raw)
        .map_err(|e| GitError::InvalidManifest(format!("invalid manifest fields: {e}")))?;

    if manifest.profile != MANIFEST_PROFILE_NAME {
        return Err(GitError::InvalidManifest(format!(
            "unsupported profile {:?}; expected {:?}",
            manifest.profile, MANIFEST_PROFILE_NAME
        )));
    }
    if manifest.canonicalization != CANONICALIZATION_ALGORITHM {
        return Err(GitError::InvalidManifest(format!(
            "unsupported canonicalization {:?}; expected {:?}",
            manifest.canonicalization, CANONICALIZATION_ALGORITHM
        )));
    }

    for (prefix, iri) in &manifest.prefixes {
        if !is_valid_prefix_label(prefix) {
            return Err(GitError::InvalidManifest(format!(
                "invalid prefix label {prefix:?}"
            )));
        }
        if oxiri::Iri::parse(iri.as_str()).is_err() {
            return Err(GitError::InvalidManifest(format!(
                "invalid prefix IRI {iri:?} for prefix {prefix:?}"
            )));
        }
    }

    let mut seen_graphs: BTreeSet<GraphKey> = BTreeSet::new();
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();
    let mut canonical_paths: BTreeSet<String> = BTreeSet::new();
    let mut prev_key: Option<GraphKey> = None;

    for entry in &manifest.graphs {
        let gname = entry.graph_name()?;
        let gkey = GraphKey::from_graph_name(&gname);
        if !seen_graphs.insert(gkey.clone()) {
            return Err(GitError::InvalidManifest(format!(
                "duplicate graph entry for {gname}"
            )));
        }
        validate_graph_path(&entry.path, manifest.syntax)?;
        if !seen_paths.insert(entry.path.clone()) {
            return Err(GitError::InvalidManifest(format!(
                "duplicate graph file path {:?}",
                entry.path
            )));
        }
        if !is_sha256_hex(&entry.sha256) {
            return Err(GitError::InvalidManifest(format!(
                "invalid sha256 digest {:?} for {}",
                entry.sha256, entry.path
            )));
        }
        if strict_layout {
            if entry.quads == 0 {
                return Err(GitError::InvalidManifest(format!(
                    "graph entry {} declares zero quads; canonical profile omits empty graphs",
                    entry.path
                )));
            }
            if let Some(prev) = &prev_key
                && gkey <= *prev
            {
                return Err(GitError::InvalidManifest(format!(
                    "manifest graph entries are not in canonical order at {}",
                    entry.path
                )));
            }
            let expected_path = assign_graph_path(&gkey, manifest.syntax, &mut canonical_paths);
            if entry.path != expected_path {
                return Err(GitError::InvalidManifest(format!(
                    "non-canonical graph path {:?}; expected {:?}",
                    entry.path, expected_path
                )));
            }
            if let GraphKey::Blank(label) = &gkey
                && !is_canonical_bnode_label(label)
            {
                return Err(GitError::InvalidManifest(format!(
                    "non-canonical blank-node graph label {label:?}; expected c14n<N>"
                )));
            }
            prev_key = Some(gkey);
        }
    }

    Ok(manifest)
}

fn is_canonical_bnode_label(label: &str) -> bool {
    let Some(rest) = label.strip_prefix("c14n") else {
        return false;
    };
    if rest == "0" {
        return true;
    }
    let bytes = rest.as_bytes();
    !bytes.is_empty()
        && (b'1'..=b'9').contains(&bytes[0])
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

/// Strictly validate a relative graph path from a manifest.
pub(crate) fn validate_graph_path(rel: &str, syntax: CanonicalSyntax) -> GitResult<()> {
    validate_relative_path(rel)?;
    let expected_prefix = format!("{GRAPHS_DIR}/");
    let Some(file_name) = rel.strip_prefix(&expected_prefix) else {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: format!("graph path must live directly under {GRAPHS_DIR}/"),
        });
    };
    if file_name.is_empty() || file_name.contains('/') {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: format!("graph path must be a single filename directly under {GRAPHS_DIR}/"),
        });
    }
    let expected_ext = format!(".{}", syntax.extension());
    if !file_name.ends_with(&expected_ext) || file_name == expected_ext {
        return Err(GitError::InvalidManifest(format!(
            "graph path {rel:?} must have extension {expected_ext} for syntax {}",
            syntax.as_str()
        )));
    }
    Ok(())
}

/// Validate that `rel` is a safe forward-slash relative path without traversal, backslashes,
/// NUL/control characters, drive prefixes, `.`, or `..`.
pub(crate) fn validate_relative_path(rel: &str) -> GitResult<()> {
    if rel.is_empty() {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: "path is empty".into(),
        });
    }
    if rel.starts_with('/') || rel.ends_with('/') {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: "path must be relative without leading or trailing slash".into(),
        });
    }
    if rel.contains('\\') {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: "backslashes are forbidden in manifest paths".into(),
        });
    }
    if rel.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: "control characters are forbidden in manifest paths".into(),
        });
    }
    for segment in rel.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(GitError::UnsafePath {
                path: rel.to_string(),
                reason: format!("disallowed path segment {segment:?}"),
            });
        }
        // Reject Windows drive-style segments like `C:`.
        if segment.contains(':') {
            return Err(GitError::UnsafePath {
                path: rel.to_string(),
                reason: format!("disallowed colon in path segment {segment:?}"),
            });
        }
    }
    let path = Path::new(rel);
    for comp in path.components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(GitError::UnsafePath {
                path: rel.to_string(),
                reason: "path must consist solely of normal relative segments".into(),
            });
        }
    }
    Ok(())
}

/// Normalize trailing slashes and `.` components so `symlink_metadata` inspects the target
/// directory entry itself rather than following a symlinked directory on Unix. Also refuses
/// any `Normal` component cancelled by `..` if that component is a symlink.
pub(crate) fn normalize_dir_for_lstat(path: &Path) -> GitResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(GitError::UnsafePath {
            path: String::new(),
            reason: "directory path is empty".into(),
        });
    }
    let mut out = PathBuf::new();
    let mut saw_normal = false;
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {
                if out.as_os_str().is_empty() {
                    out.push(".");
                }
            }
            Component::Normal(s) => {
                if out == Path::new(".") {
                    out.clear();
                }
                out.push(s);
                saw_normal = true;
            }
            Component::ParentDir => {
                if saw_normal && matches!(out.components().next_back(), Some(Component::Normal(_)))
                {
                    match fs::symlink_metadata(&out) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            return Err(GitError::UnsafePath {
                                path: path.display().to_string(),
                                reason: "path traverses a symlink component before '..'".into(),
                            });
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(GitError::io(&out, e)),
                    }
                    out.pop();
                    if !out.as_os_str().is_empty()
                        && matches!(out.components().next_back(), Some(Component::Normal(_)))
                    {
                        match fs::symlink_metadata(&out) {
                            Ok(meta) if meta.file_type().is_symlink() => {
                                return Err(GitError::UnsafePath {
                                    path: path.display().to_string(),
                                    reason: "resolved directory target must not be a symlink"
                                        .into(),
                                });
                            }
                            Ok(_) => {}
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(e) => return Err(GitError::io(&out, e)),
                        }
                    }
                } else {
                    if out == Path::new(".") {
                        out.clear();
                    }
                    out.push("..");
                }
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Ok(out)
}

/// Resolve the parent directory of a file path, mapping a bare filename like `mapping.json`
/// to `"."` and rejecting trailing slashes or directory-only spellings.
pub(crate) fn effective_parent(path: &Path) -> GitResult<PathBuf> {
    reject_trailing_sep_on_file_path(path)?;
    if path.file_name().is_none() {
        return Err(GitError::UnsafePath {
            path: path.display().to_string(),
            reason: "destination path must name a file".into(),
        });
    }
    match path.parent() {
        None => Err(GitError::UnsafePath {
            path: path.display().to_string(),
            reason: "destination path has no parent directory".into(),
        }),
        Some(p) if p.as_os_str().is_empty() => Ok(PathBuf::from(".")),
        Some(p) => normalize_dir_for_lstat(p),
    }
}

fn reject_trailing_sep_on_file_path(path: &Path) -> GitResult<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(GitError::UnsafePath {
            path: String::new(),
            reason: "destination file path is empty".into(),
        });
    }
    if bytes.ends_with(b"/") || (cfg!(windows) && bytes.ends_with(b"\\")) {
        return Err(GitError::UnsafePath {
            path: path.display().to_string(),
            reason: "destination file path must not end with a directory separator".into(),
        });
    }
    Ok(())
}

/// Check `dir` (without creating it) and report whether it already exists. Refuses symlinks
/// on `dir` (including trailing `/` or `/.`) or on its deepest existing ancestor.
fn preflight_target_dir_no_symlink(dir: &Path) -> GitResult<(PathBuf, bool)> {
    let clean = normalize_dir_for_lstat(dir)?;
    match fs::symlink_metadata(&clean) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target directory must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target path is not a directory".into(),
                });
            }
            Ok((clean, true))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            verify_deepest_existing_ancestor_no_symlink(&clean)?;
            Ok((clean, false))
        }
        Err(e) => Err(GitError::io(&clean, e)),
    }
}

fn preflight_optional_child_dir_no_symlink(parent: &Path, child: &str) -> GitResult<()> {
    let full = parent.join(child);
    match fs::symlink_metadata(&full) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: child.to_string(),
                    reason: format!("managed directory {child:?} must not be a symlink"),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: child.to_string(),
                    reason: format!("managed path {child:?} is not a directory"),
                });
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(GitError::io(&full, e)),
    }
}

fn verify_deepest_existing_ancestor_no_symlink(clean: &Path) -> GitResult<PathBuf> {
    let mut cur = clean.to_path_buf();
    loop {
        match cur.parent() {
            None => return Ok(cur),
            Some(p) if p.as_os_str().is_empty() => {
                let dot = PathBuf::from(".");
                verify_existing_dir_no_symlink(&dot, ".")?;
                return Ok(dot);
            }
            Some(p) => {
                cur = p.to_path_buf();
                match fs::symlink_metadata(&cur) {
                    Ok(meta) => {
                        if meta.file_type().is_symlink() {
                            return Err(GitError::UnsafePath {
                                path: cur.display().to_string(),
                                reason: "ancestor directory must not be a symlink".into(),
                            });
                        }
                        if !meta.file_type().is_dir() {
                            return Err(GitError::UnsafePath {
                                path: cur.display().to_string(),
                                reason: "ancestor path is not a directory".into(),
                            });
                        }
                        return Ok(cur);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(GitError::io(&cur, e)),
                }
            }
        }
    }
}

/// Ensure `dir` exists (creating missing components one by one without following symlinks)
/// and is not a symlink (even when passed with trailing `/` or `/.`), returning its
/// normalized path.
pub(crate) fn ensure_dir_no_symlink(dir: &Path) -> GitResult<PathBuf> {
    let clean = normalize_dir_for_lstat(dir)?;
    match fs::symlink_metadata(&clean) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target directory must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target path is not a directory".into(),
                });
            }
            Ok(clean)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut missing: Vec<std::ffi::OsString> = Vec::new();
            let mut probe = clean.clone();
            let base = loop {
                match fs::symlink_metadata(&probe) {
                    Ok(meta) => {
                        if meta.file_type().is_symlink() {
                            return Err(GitError::UnsafePath {
                                path: probe.display().to_string(),
                                reason: "ancestor directory must not be a symlink".into(),
                            });
                        }
                        if !meta.file_type().is_dir() {
                            return Err(GitError::UnsafePath {
                                path: probe.display().to_string(),
                                reason: "ancestor path is not a directory".into(),
                            });
                        }
                        break probe;
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        let Some(name) = probe.file_name() else {
                            return Err(GitError::io(&probe, err));
                        };
                        missing.push(name.to_os_string());
                        match probe.parent() {
                            Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
                            _ => {
                                let dot = PathBuf::from(".");
                                verify_existing_dir_no_symlink(&dot, ".")?;
                                break dot;
                            }
                        }
                    }
                    Err(err) => return Err(GitError::io(&probe, err)),
                }
            };
            let mut cur = base;
            for seg in missing.into_iter().rev() {
                cur.push(seg);
                match fs::create_dir(&cur) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        verify_existing_dir_no_symlink(&cur, &cur.display().to_string())?;
                    }
                    Err(err) => return Err(GitError::io(&cur, err)),
                }
            }
            Ok(clean)
        }
        Err(e) => Err(GitError::io(&clean, e)),
    }
}

/// Verify an already-existing directory is not a symlink (including trailing `/` or `/.`),
/// returning its normalized path.
pub(crate) fn verify_existing_dir_no_symlink(dir: &Path, label: &str) -> GitResult<PathBuf> {
    let clean = normalize_dir_for_lstat(dir)?;
    let meta = fs::symlink_metadata(&clean).map_err(|e| GitError::io(&clean, e))?;
    if meta.file_type().is_symlink() {
        return Err(GitError::UnsafePath {
            path: label.to_string(),
            reason: "directory must not be a symlink".into(),
        });
    }
    if !meta.file_type().is_dir() {
        return Err(GitError::UnsafePath {
            path: label.to_string(),
            reason: "path is not a directory".into(),
        });
    }
    Ok(clean)
}

/// Ensure immediate child directory `child` under an already verified `parent` exists and is
/// not a symlink.
pub(crate) fn ensure_child_dir_no_symlink(parent: &Path, child: &str) -> GitResult<PathBuf> {
    let clean_parent = normalize_dir_for_lstat(parent)?;
    let full = clean_parent.join(child);
    match fs::symlink_metadata(&full) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: child.to_string(),
                    reason: format!("managed directory {child:?} must not be a symlink"),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: child.to_string(),
                    reason: format!("managed path {child:?} is not a directory"),
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&full).map_err(|e| GitError::io(&full, e))?;
        }
        Err(e) => return Err(GitError::io(&full, e)),
    }
    Ok(full)
}

/// Resolve `rel` inside `root`, verifying that no component beneath `root` is a symlink.
pub(crate) fn resolve_safe_path(root: &Path, rel: &str) -> GitResult<PathBuf> {
    validate_relative_path(rel)?;
    let mut cur = normalize_dir_for_lstat(root)?;
    for segment in rel.split('/') {
        cur.push(segment);
        match fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: rel.to_string(),
                        reason: format!("symlink component {segment:?} is not allowed"),
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&cur, e)),
        }
    }
    Ok(cur)
}

/// Open an existing regular file without following symlinks and without blocking on FIFOs.
pub(crate) fn open_regular_file_no_symlink(path: &Path, label: &str) -> GitResult<(File, u64)> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    GitError::UnsafePath {
                        path: label.to_string(),
                        reason: "symlink is not allowed".into(),
                    }
                } else {
                    GitError::io(path, e)
                }
            })?
    };

    #[cfg(not(unix))]
    let file = {
        let meta = fs::symlink_metadata(path).map_err(|e| GitError::io(path, e))?;
        if meta.file_type().is_symlink() {
            return Err(GitError::UnsafePath {
                path: label.to_string(),
                reason: "symlink is not allowed".into(),
            });
        }
        File::open(path).map_err(|e| GitError::io(path, e))?
    };

    let meta = file.metadata().map_err(|e| GitError::io(path, e))?;
    if !meta.file_type().is_file() {
        return Err(GitError::UnsafePath {
            path: label.to_string(),
            reason: "path must be a regular file".into(),
        });
    }
    Ok((file, meta.len()))
}

/// Read an existing regular file into memory with an explicit byte limit, rejecting symlinks
/// and special files (such as FIFOs).
pub(crate) fn read_bounded_regular_file(
    path: &Path,
    label: &str,
    max_bytes: u64,
) -> GitResult<Vec<u8>> {
    let (file, len) = open_regular_file_no_symlink(path, label)?;
    if len > max_bytes {
        return Err(GitError::InvalidManifest(format!(
            "{label} exceeds maximum allowed size ({max_bytes} bytes)"
        )));
    }
    let mut out = Vec::with_capacity(len.min(1 << 20) as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| GitError::io(path, e))?;
    if out.len() as u64 > max_bytes {
        return Err(GitError::InvalidManifest(format!(
            "{label} exceeds maximum allowed size ({max_bytes} bytes)"
        )));
    }
    Ok(out)
}

/// Atomically write `bytes` to `path` via an exclusively created temporary file in the same
/// directory, refusing symlinks on `path` and its parent directory.
pub(crate) fn write_atomic_safe(path: &Path, bytes: &[u8]) -> GitResult<()> {
    let parent = effective_parent(path)?;
    let (temp_path, _, _) =
        stage_atomic_stream_hashed(path, &path.display().to_string(), u64::MAX, |w| {
            w.write_all(bytes).map_err(|e| GitError::io(path, e))
        })?;
    check_dest_file_not_symlink(path)?;
    temp_path
        .persist(path)
        .map_err(|e| GitError::io(path, e.error))?;
    fsutil::sync_dir(&parent).map_err(|e| GitError::io(&parent, e))?;
    Ok(())
}

/// Stream serialized bytes to an exclusively created temporary file (`O_CREAT | O_EXCL`)
/// in `path`'s parent directory while enforcing `max_bytes` and computing its lowercase hex
/// SHA-256 digest. Closes the file descriptor and returns `(TempPath, bytes_written, sha256)`
/// so callers can validate all files before atomically persisting them.
fn stage_atomic_stream_hashed(
    path: &Path,
    label: &str,
    max_bytes: u64,
    write_fn: impl FnOnce(&mut HashingWriter<BufWriter<&mut File>>) -> GitResult<()>,
) -> GitResult<(tempfile::TempPath, u64, String)> {
    let parent = effective_parent(path)?;
    verify_existing_dir_no_symlink(&parent, &parent.display().to_string())?;
    check_dest_file_not_symlink(path)?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".sparkles-tmp-")
        .tempfile_in(&parent)
        .map_err(|e| GitError::io(&parent, e))?;

    let (written, digest) = {
        let bw = BufWriter::new(tmp.as_file_mut());
        let mut hw = HashingWriter::new(bw, max_bytes, label.to_string());
        write_fn(&mut hw).map_err(|e| map_write_limit_err(e, label, max_bytes))?;
        hw.flush()
            .map_err(|e| map_io_write_limit_err(path, e, label, max_bytes))?;
        hw.finish()
    };
    tmp.as_file()
        .sync_all()
        .map_err(|e| GitError::io(path, e))?;

    Ok((tmp.into_temp_path(), written, digest))
}

fn map_write_limit_err(err: GitError, label: &str, max_bytes: u64) -> GitError {
    if let GitError::Io { ref source, .. } = err
        && source.kind() == io::ErrorKind::FileTooLarge
    {
        return GitError::Format(format!(
            "{label} exceeds maximum allowed write size ({max_bytes} bytes)"
        ));
    }
    err
}

fn map_io_write_limit_err(path: &Path, err: io::Error, label: &str, max_bytes: u64) -> GitError {
    if err.kind() == io::ErrorKind::FileTooLarge {
        GitError::Format(format!(
            "{label} exceeds maximum allowed write size ({max_bytes} bytes)"
        ))
    } else {
        GitError::io(path, err)
    }
}

pub(crate) fn check_dest_file_not_symlink(path: &Path) -> GitResult<()> {
    reject_trailing_sep_on_file_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: path.display().to_string(),
                    reason: "destination file must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: path.display().to_string(),
                    reason: "destination path is not a regular file".into(),
                });
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(GitError::io(path, e)),
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256Stream,
    written: u64,
    max_bytes: u64,
    label: String,
}

impl<W> HashingWriter<W> {
    fn new(inner: W, max_bytes: u64, label: String) -> Self {
        Self {
            inner,
            hasher: Sha256Stream::new(),
            written: 0,
            max_bytes,
            label,
        }
    }

    fn finish(self) -> (u64, String) {
        (self.written, self.hasher.finalize_hex())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let next = self.written.saturating_add(buf.len() as u64);
        if next > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "{} exceeds maximum allowed write size ({} bytes)",
                    self.label, self.max_bytes
                ),
            ));
        }
        let n = self.inner.write(buf)?;
        self.written = self.written.saturating_add(n as u64);
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct BoundedHashingReader<R> {
    inner: R,
    hasher: Sha256Stream,
    bytes_read: u64,
    max_bytes: u64,
    label: String,
}

impl<R: Read> BoundedHashingReader<R> {
    fn new(inner: R, max_bytes: u64, label: String) -> Self {
        Self {
            inner,
            hasher: Sha256Stream::new(),
            bytes_read: 0,
            max_bytes,
            label,
        }
    }

    fn drain_to_eof(&mut self) -> GitResult<()> {
        let mut buf = [0u8; 8192];
        loop {
            let n = self.read(&mut buf).map_err(|e| {
                if e.kind() == io::ErrorKind::FileTooLarge {
                    GitError::InvalidManifest(e.to_string())
                } else {
                    GitError::Io {
                        path: Some(PathBuf::from(&self.label)),
                        source: e,
                    }
                }
            })?;
            if n == 0 {
                break;
            }
        }
        Ok(())
    }

    fn finish(self) -> (u64, String) {
        (self.bytes_read, self.hasher.finalize_hex())
    }
}

impl<R: Read> Read for BoundedHashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes_read = self.bytes_read.saturating_add(n as u64);
            if self.bytes_read > self.max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    format!(
                        "{} exceeds maximum allowed size ({} bytes)",
                        self.label, self.max_bytes
                    ),
                ));
            }
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

fn assign_graph_path(
    key: &GraphKey,
    syntax: CanonicalSyntax,
    used: &mut BTreeSet<String>,
) -> String {
    let ext = syntax.extension();
    match key {
        GraphKey::Default => {
            let p = format!("{GRAPHS_DIR}/default.{ext}");
            used.insert(p.clone());
            p
        }
        GraphKey::Named(iri) => {
            let slug = slug_from_iri(iri);
            let hash = sha256_hex(iri.as_bytes());
            for len in [16usize, 24, 32, 64] {
                let candidate = format!("{GRAPHS_DIR}/{slug}-{}.{ext}", &hash[..len]);
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            unreachable!("distinct graph IRIs cannot share a 64-hex SHA-256")
        }
        GraphKey::Blank(label) => {
            let slug = sanitize_slug(label);
            let hash = sha256_hex(label.as_bytes());
            for len in [16usize, 24, 32, 64] {
                let candidate = format!("{GRAPHS_DIR}/bnode-{slug}-{}.{ext}", &hash[..len]);
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            unreachable!("distinct blank-node labels cannot share a 64-hex SHA-256")
        }
    }
}

fn slug_from_iri(iri: &str) -> String {
    let tail = iri
        .rsplit(['/', '#', ':'])
        .find(|seg| !seg.is_empty())
        .unwrap_or("graph");
    let slug = sanitize_slug(tail);
    if slug.is_empty() || slug == "default" {
        "graph".to_string()
    } else {
        slug
    }
}

pub(crate) fn sanitize_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        if out.len() >= 32 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch == '-' || ch == '_') && !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "graph".to_string()
    } else {
        trimmed
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256Stream::new();
    h.update(bytes);
    h.finalize_hex()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ------------------------------------------------------------ canonicalization ------

/// Bounded, CDT-aware dataset-wide RDFC-1.0 canonicalization.
///
/// Blank nodes referenced inside Sparkles' `cdt:List` and `cdt:Map` composite literals are
/// decomposed into order-independent auxiliary quads before RDFC-1.0 runs so cross-graph
/// blank-node identity between ordinary RDF positions and composite literals is preserved
/// losslessly and deterministically.
fn canonicalize_dataset_bounded(
    quads: impl IntoIterator<Item = Quad>,
    max_quads: usize,
    max_blank_nodes: usize,
    max_ambiguous_group: usize,
) -> GitResult<RdfDataset> {
    let mut collected: Vec<Quad> = Vec::new();
    let mut has_cdt_bnode = false;
    for q in quads {
        if collected.len() >= max_quads {
            return Err(GitError::Format(format!(
                "dataset exceeds canonicalization quad limit ({max_quads})"
            )));
        }
        if term_has_cdt_bnode(&q.object) {
            has_cdt_bnode = true;
        }
        collected.push(q);
    }

    if !has_cdt_bnode {
        check_canonicalization_budget(&collected, max_blank_nodes, max_ambiguous_group)?;
        let mut rdf_dataset: RdfDataset = collected.into_iter().collect();
        rdf_dataset.canonicalize(RDFC10);
        return Ok(rdf_dataset);
    }

    canonicalize_with_cdt_links(collected, max_quads, max_blank_nodes, max_ambiguous_group)
}

fn term_has_cdt_bnode(t: &Term) -> bool {
    match t {
        Term::Literal(l) => cdt::may_name_bnodes(l),
        Term::Triple(tr) => term_has_cdt_bnode(&tr.object),
        _ => false,
    }
}

const CDT_SYNTH_NS: &str = "urn:sparkles-internal:git-cdt-canon:";

struct CdtEncoder {
    synth_graph: GraphName,
    kind_pred: NamedNode,
    list_kind: Term,
    map_kind: Term,
    null_marker: Term,
    entry_pred: NamedNode,
    key_pred: NamedNode,
    val_pred: NamedNode,
    null_val_pred: NamedNode,
    used_labels: HashSet<String>,
    next_id: u64,
    synth_quads: Vec<Quad>,
    base_quads: usize,
    max_quads: usize,
    max_blank_nodes: usize,
}

impl CdtEncoder {
    fn new(quads: &[Quad], max_quads: usize, max_blank_nodes: usize) -> GitResult<Self> {
        let mut used_labels = HashSet::new();
        let mut existing_graphs = HashSet::new();
        for q in quads {
            collect_quad_bnodes(q, &mut used_labels);
            if used_labels.len() > max_blank_nodes {
                return Err(GitError::Format(format!(
                    "dataset exceeds canonicalization blank-node limit ({max_blank_nodes})"
                )));
            }
            if let GraphName::NamedNode(n) = &q.graph_name {
                existing_graphs.insert(n.as_str().to_string());
            }
        }
        let mut synth_graph_iri = format!("{CDT_SYNTH_NS}graph");
        let mut suffix = 0u64;
        while existing_graphs.contains(&synth_graph_iri) {
            suffix += 1;
            synth_graph_iri = format!("{CDT_SYNTH_NS}graph-{suffix}");
        }
        Ok(Self {
            synth_graph: GraphName::NamedNode(NamedNode::new_unchecked(synth_graph_iri)),
            kind_pred: NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}kind")),
            list_kind: Term::NamedNode(NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}list"))),
            map_kind: Term::NamedNode(NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}map"))),
            // Only ever placed as the object of `null-idx/{idx}` or `null-val` inside
            // `synth_graph`; real RDF terms always use `idx/{idx}` / `val` so no RDF IRI
            // can ever collide with CDT null.
            null_marker: Term::NamedNode(NamedNode::new_unchecked(format!(
                "{CDT_SYNTH_NS}null-marker"
            ))),
            entry_pred: NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}entry")),
            key_pred: NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}key")),
            val_pred: NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}val")),
            null_val_pred: NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}null-val")),
            used_labels,
            next_id: 0,
            synth_quads: Vec::new(),
            base_quads: quads.len(),
            max_quads,
            max_blank_nodes,
        })
    }

    fn check_quad_headroom(&self, additional: usize) -> GitResult<()> {
        let total = self
            .base_quads
            .saturating_add(self.synth_quads.len())
            .saturating_add(additional);
        if total > self.max_quads {
            return Err(GitError::Format(format!(
                "dataset exceeds canonicalization quad limit ({}) during composite literal expansion",
                self.max_quads
            )));
        }
        Ok(())
    }

    fn check_bnode_headroom(&self, additional: usize) -> GitResult<()> {
        let total = self.used_labels.len().saturating_add(additional);
        if total > self.max_blank_nodes {
            return Err(GitError::Format(format!(
                "dataset exceeds canonicalization blank-node limit ({}) during composite literal expansion",
                self.max_blank_nodes
            )));
        }
        Ok(())
    }

    fn push_synth_quad(&mut self, quad: Quad) -> GitResult<()> {
        self.check_quad_headroom(1)?;
        self.synth_quads.push(quad);
        Ok(())
    }

    fn fresh_bnode(&mut self) -> GitResult<BlankNode> {
        self.check_bnode_headroom(1)?;
        loop {
            let candidate = format!("spkcdt{}", self.next_id);
            self.next_id += 1;
            if self.used_labels.insert(candidate.clone()) {
                return Ok(BlankNode::new_unchecked(candidate));
            }
        }
    }

    fn encode_term(&mut self, t: Term) -> GitResult<Term> {
        match t {
            Term::Literal(ref lit) if cdt::may_name_bnodes(lit) => {
                let dt = lit.datatype().as_str();
                if dt == cdt::LIST
                    && let Some(elems) = cdt::parse_list(lit.value())
                {
                    self.check_bnode_headroom(1)?;
                    self.check_quad_headroom(1usize.saturating_add(elems.len()))?;
                    let holder = self.fresh_bnode()?;
                    self.push_synth_quad(Quad::new(
                        holder.clone(),
                        self.kind_pred.clone(),
                        self.list_kind.clone(),
                        self.synth_graph.clone(),
                    ))?;
                    for (idx, elem) in elems.into_iter().enumerate() {
                        match elem {
                            cdt::Elem::Null => {
                                let null_pred = NamedNode::new_unchecked(format!(
                                    "{CDT_SYNTH_NS}null-idx/{idx}"
                                ));
                                self.push_synth_quad(Quad::new(
                                    holder.clone(),
                                    null_pred,
                                    self.null_marker.clone(),
                                    self.synth_graph.clone(),
                                ))?;
                            }
                            cdt::Elem::Term(inner) => {
                                let encoded = self.encode_term(inner)?;
                                let idx_pred =
                                    NamedNode::new_unchecked(format!("{CDT_SYNTH_NS}idx/{idx}"));
                                self.push_synth_quad(Quad::new(
                                    holder.clone(),
                                    idx_pred,
                                    encoded,
                                    self.synth_graph.clone(),
                                ))?;
                            }
                        }
                    }
                    return Ok(Term::BlankNode(holder));
                }
                if dt == cdt::MAP
                    && let Some(entries) = cdt::parse_map(lit.value())
                {
                    self.check_bnode_headroom(1usize.saturating_add(entries.len()))?;
                    self.check_quad_headroom(
                        1usize.saturating_add(entries.len().saturating_mul(3)),
                    )?;
                    let holder = self.fresh_bnode()?;
                    self.push_synth_quad(Quad::new(
                        holder.clone(),
                        self.kind_pred.clone(),
                        self.map_kind.clone(),
                        self.synth_graph.clone(),
                    ))?;
                    for (k, v) in entries {
                        let entry_bn = self.fresh_bnode()?;
                        let enc_k = self.encode_term(k)?;
                        self.push_synth_quad(Quad::new(
                            holder.clone(),
                            self.entry_pred.clone(),
                            entry_bn.clone(),
                            self.synth_graph.clone(),
                        ))?;
                        self.push_synth_quad(Quad::new(
                            entry_bn.clone(),
                            self.key_pred.clone(),
                            enc_k,
                            self.synth_graph.clone(),
                        ))?;
                        match v {
                            cdt::Elem::Null => {
                                self.push_synth_quad(Quad::new(
                                    entry_bn,
                                    self.null_val_pred.clone(),
                                    self.null_marker.clone(),
                                    self.synth_graph.clone(),
                                ))?;
                            }
                            cdt::Elem::Term(inner) => {
                                let enc_v = self.encode_term(inner)?;
                                self.push_synth_quad(Quad::new(
                                    entry_bn,
                                    self.val_pred.clone(),
                                    enc_v,
                                    self.synth_graph.clone(),
                                ))?;
                            }
                        }
                    }
                    return Ok(Term::BlankNode(holder));
                }
                Ok(t)
            }
            Term::Triple(tr) => {
                let obj = self.encode_term(tr.object)?;
                Ok(Term::Triple(Box::new(Triple::new(
                    tr.subject,
                    tr.predicate,
                    obj,
                ))))
            }
            other => Ok(other),
        }
    }
}

fn canonicalize_with_cdt_links(
    quads: Vec<Quad>,
    max_quads: usize,
    max_blank_nodes: usize,
    max_ambiguous_group: usize,
) -> GitResult<RdfDataset> {
    let mut enc = CdtEncoder::new(&quads, max_quads, max_blank_nodes)?;
    let mut augmented = Vec::with_capacity(quads.len());
    for q in quads {
        let obj = enc.encode_term(q.object)?;
        augmented.push(Quad::new(q.subject, q.predicate, obj, q.graph_name));
    }
    let synth_graph = enc.synth_graph.clone();
    let kind_pred = enc.kind_pred.clone();
    let list_kind = enc.list_kind.clone();
    let map_kind = enc.map_kind.clone();
    let entry_pred = enc.entry_pred.clone();
    let key_pred = enc.key_pred.clone();
    let val_pred = enc.val_pred.clone();
    let null_val_pred = enc.null_val_pred.clone();

    if enc.synth_quads.is_empty() {
        // Every candidate CDT literal was ill-formed, so no links exist.
        check_canonicalization_budget(&augmented, max_blank_nodes, max_ambiguous_group)?;
        let mut rdf_dataset: RdfDataset = augmented.into_iter().collect();
        rdf_dataset.canonicalize(RDFC10);
        return Ok(rdf_dataset);
    }

    augmented.extend(enc.synth_quads);
    if augmented.len() > max_quads {
        return Err(GitError::Format(format!(
            "dataset exceeds canonicalization quad limit ({max_quads}) during composite literal expansion"
        )));
    }
    check_canonicalization_budget(&augmented, max_blank_nodes, max_ambiguous_group)?;

    let mut rdf_dataset: RdfDataset = augmented.into_iter().collect();
    rdf_dataset.canonicalize(RDFC10);

    // Partition canonicalized quads back into real quads and synthetic CDT descriptors.
    let mut real_quads: Vec<Quad> = Vec::new();
    let mut cdt_kinds: HashMap<String, bool> = HashMap::new(); // true = list, false = map
    let mut list_elems: HashMap<String, BTreeMap<usize, Option<Term>>> = HashMap::new();
    let mut map_entries: HashMap<String, Vec<String>> = HashMap::new();
    let mut entry_keys: HashMap<String, Term> = HashMap::new();
    let mut entry_vals: HashMap<String, Option<Term>> = HashMap::new();
    let mut synth_bnodes: HashSet<String> = HashSet::new();

    let idx_prefix = format!("{CDT_SYNTH_NS}idx/");
    let null_idx_prefix = format!("{CDT_SYNTH_NS}null-idx/");

    for qref in rdf_dataset.iter() {
        let q = qref.into_owned();
        if q.graph_name != synth_graph {
            real_quads.push(q);
            continue;
        }
        let NamedOrBlankNode::BlankNode(subj_bn) = q.subject else {
            continue;
        };
        let subj = subj_bn.as_str().to_string();
        synth_bnodes.insert(subj.clone());

        if q.predicate == kind_pred {
            if q.object == list_kind {
                cdt_kinds.insert(subj, true);
            } else if q.object == map_kind {
                cdt_kinds.insert(subj, false);
            }
        } else if let Some(idx_str) = q.predicate.as_str().strip_prefix(&idx_prefix) {
            if let Ok(idx) = idx_str.parse::<usize>() {
                list_elems
                    .entry(subj)
                    .or_default()
                    .insert(idx, Some(q.object));
            }
        } else if let Some(idx_str) = q.predicate.as_str().strip_prefix(&null_idx_prefix) {
            if let Ok(idx) = idx_str.parse::<usize>() {
                list_elems.entry(subj).or_default().insert(idx, None);
            }
        } else if q.predicate == entry_pred {
            if let Term::BlankNode(ebn) = q.object {
                let elabel = ebn.as_str().to_string();
                synth_bnodes.insert(elabel.clone());
                map_entries.entry(subj).or_default().push(elabel);
            }
        } else if q.predicate == key_pred {
            entry_keys.insert(subj, q.object);
        } else if q.predicate == val_pred {
            entry_vals.insert(subj, Some(q.object));
        } else if q.predicate == null_val_pred {
            entry_vals.insert(subj, None);
        }
    }

    // Build compact renumbering for all surviving real blank nodes in their canonical order
    // so synthetic placeholder blank nodes do not leave gaps in `c14n0, c14n1, ...`.
    let mut real_bnode_labels: Vec<String> = Vec::new();
    let mut seen_real: HashSet<String> = HashSet::new();
    for q in &real_quads {
        collect_real_bnodes_from_quad(q, &synth_bnodes, &mut seen_real, &mut real_bnode_labels);
    }
    for elems in list_elems.values() {
        for t in elems.values().flatten() {
            collect_real_bnodes_from_term(t, &synth_bnodes, &mut seen_real, &mut real_bnode_labels);
        }
    }
    for t in entry_keys
        .values()
        .chain(entry_vals.values().filter_map(Option::as_ref))
    {
        collect_real_bnodes_from_term(t, &synth_bnodes, &mut seen_real, &mut real_bnode_labels);
    }
    real_bnode_labels.sort_by(|a, b| c14n_sort_key(a).cmp(&c14n_sort_key(b)));
    let remap: HashMap<String, BlankNode> = real_bnode_labels
        .into_iter()
        .enumerate()
        .map(|(i, old)| (old, BlankNode::new_unchecked(format!("c14n{i}"))))
        .collect();

    struct CdtDecoder<'a> {
        cdt_kinds: &'a HashMap<String, bool>,
        list_elems: &'a HashMap<String, BTreeMap<usize, Option<Term>>>,
        map_entries: &'a HashMap<String, Vec<String>>,
        entry_keys: &'a HashMap<String, Term>,
        entry_vals: &'a HashMap<String, Option<Term>>,
        remap: &'a HashMap<String, BlankNode>,
    }

    impl CdtDecoder<'_> {
        fn decode_term(&self, t: Term) -> GitResult<Term> {
            match t {
                Term::BlankNode(bn) => {
                    let label = bn.as_str();
                    if let Some(&is_list) = self.cdt_kinds.get(label) {
                        if is_list {
                            let elems_map = self.list_elems.get(label);
                            let mut elems = Vec::new();
                            if let Some(m) = elems_map {
                                for term_opt in m.values() {
                                    elems.push(self.decode_elem(term_opt.clone())?);
                                }
                            }
                            let lex = cdt::list_lexical(&elems);
                            return Ok(Term::Literal(Literal::new_typed_literal(
                                lex,
                                NamedNode::new_unchecked(cdt::LIST),
                            )));
                        } else {
                            let mut pairs: Vec<(Term, cdt::Elem)> = Vec::new();
                            if let Some(eids) = self.map_entries.get(label) {
                                for eid in eids {
                                    let k = self
                                        .entry_keys
                                        .get(eid)
                                        .ok_or_else(|| {
                                            GitError::Format(
                                                "missing key while reconstructing cdt:Map".into(),
                                            )
                                        })?
                                        .clone();
                                    let v = self
                                        .entry_vals
                                        .get(eid)
                                        .ok_or_else(|| {
                                            GitError::Format(
                                                "missing value while reconstructing cdt:Map".into(),
                                            )
                                        })?
                                        .clone();
                                    pairs.push((self.decode_term(k)?, self.decode_elem(v)?));
                                }
                            }
                            // Sort entries directly in Sparkles' canonical `cdt::key_cmp` order.
                            pairs.sort_by(|a, b| cdt::key_cmp(&a.0, &b.0));
                            let canonical_lex = cdt::map_lexical(&pairs);
                            return Ok(Term::Literal(Literal::new_typed_literal(
                                canonical_lex,
                                NamedNode::new_unchecked(cdt::MAP),
                            )));
                        }
                    }
                    let mapped = self.remap.get(label).cloned().unwrap_or(bn);
                    Ok(Term::BlankNode(mapped))
                }
                Term::Triple(tr) => {
                    let s = match tr.subject {
                        NamedOrBlankNode::BlankNode(bn) => NamedOrBlankNode::BlankNode(
                            self.remap.get(bn.as_str()).cloned().unwrap_or(bn),
                        ),
                        n => n,
                    };
                    let o = self.decode_term(tr.object)?;
                    Ok(Term::Triple(Box::new(Triple::new(s, tr.predicate, o))))
                }
                other => Ok(other),
            }
        }

        fn decode_elem(&self, t: Option<Term>) -> GitResult<cdt::Elem> {
            match t {
                None => Ok(cdt::Elem::Null),
                Some(term) => Ok(cdt::Elem::Term(self.decode_term(term)?)),
            }
        }
    }

    let dec = CdtDecoder {
        cdt_kinds: &cdt_kinds,
        list_elems: &list_elems,
        map_entries: &map_entries,
        entry_keys: &entry_keys,
        entry_vals: &entry_vals,
        remap: &remap,
    };

    let mut out = RdfDataset::new();
    for q in real_quads {
        let s = match q.subject {
            NamedOrBlankNode::BlankNode(bn) => {
                NamedOrBlankNode::BlankNode(remap.get(bn.as_str()).cloned().unwrap_or(bn))
            }
            n => n,
        };
        let o = dec.decode_term(q.object)?;
        let g = match q.graph_name {
            GraphName::BlankNode(bn) => {
                GraphName::BlankNode(remap.get(bn.as_str()).cloned().unwrap_or(bn))
            }
            other => other,
        };
        out.insert(Quad::new(s, q.predicate, o, g).as_ref());
    }
    Ok(out)
}

fn c14n_sort_key(label: &str) -> (u8, u64, &str) {
    if let Some(rest) = label.strip_prefix("c14n")
        && let Ok(n) = rest.parse::<u64>()
    {
        return (0, n, label);
    }
    (1, 0, label)
}

fn collect_quad_bnodes(q: &Quad, out: &mut HashSet<String>) {
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        out.insert(b.as_str().to_string());
    }
    collect_term_bnodes(&q.object, out);
    if let GraphName::BlankNode(b) = &q.graph_name {
        out.insert(b.as_str().to_string());
    }
}

fn collect_term_bnodes(t: &Term, out: &mut HashSet<String>) {
    match t {
        Term::BlankNode(b) => {
            out.insert(b.as_str().to_string());
        }
        Term::Triple(tr) => {
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                out.insert(b.as_str().to_string());
            }
            collect_term_bnodes(&tr.object, out);
        }
        Term::Literal(l) if cdt::may_name_bnodes(l) => {
            let _ = cdt::relabel_literal(l, &mut |b| {
                out.insert(b.to_string());
                b.to_string()
            });
        }
        _ => {}
    }
}

fn collect_real_bnodes_from_quad(
    q: &Quad,
    synth: &HashSet<String>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        let s = b.as_str();
        if !synth.contains(s) && seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    }
    collect_real_bnodes_from_term(&q.object, synth, seen, out);
    if let GraphName::BlankNode(b) = &q.graph_name {
        let s = b.as_str();
        if !synth.contains(s) && seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    }
}

fn collect_real_bnodes_from_term(
    t: &Term,
    synth: &HashSet<String>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    match t {
        Term::BlankNode(b) => {
            let s = b.as_str();
            if !synth.contains(s) && seen.insert(s.to_string()) {
                out.push(s.to_string());
            }
        }
        Term::Triple(tr) => {
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                let s = b.as_str();
                if !synth.contains(s) && seen.insert(s.to_string()) {
                    out.push(s.to_string());
                }
            }
            collect_real_bnodes_from_term(&tr.object, synth, seen, out);
        }
        _ => {}
    }
}

/// Fast preflight guard before invoking `oxrdf::Dataset::canonicalize(RDFC10)`.
///
/// `oxrdf`'s RDFC-1.0 implementation materializes all `k!` permutations of indistinguishable
/// adjacent blank nodes in `hash_n_degree_quads`. This preflight computes 1-hop signatures
/// (matching RDFC-1.0's `hash_first_degree_quads`) and refuses inputs that exceed the
/// blank-node count, adjacent indistinguishable neighbor cap, or ambiguous connected
/// component cap before `oxrdf` can allocate factorial-sized permutation vectors.
fn check_canonicalization_budget(
    quads: &[Quad],
    max_blank_nodes: usize,
    max_ambiguous_group: usize,
) -> GitResult<()> {
    const ROLE_S: u8 = 0;
    const ROLE_O: u8 = 1;
    const ROLE_G: u8 = 2;
    const ROLE_OS: u8 = 3;
    const ROLE_OO: u8 = 4;

    let mut bnode_ids: HashMap<&str, usize> = HashMap::new();
    let mut incident: Vec<Vec<usize>> = Vec::new();

    fn intern_bnode<'a>(
        label: &'a str,
        bnode_ids: &mut HashMap<&'a str, usize>,
        incident: &mut Vec<Vec<usize>>,
        qidx: usize,
        max_blank_nodes: usize,
    ) -> GitResult<()> {
        let next = bnode_ids.len();
        let id = if let Some(&id) = bnode_ids.get(label) {
            id
        } else {
            if next >= max_blank_nodes {
                return Err(GitError::Format(format!(
                    "dataset exceeds canonicalization blank-node limit ({max_blank_nodes})"
                )));
            }
            bnode_ids.insert(label, next);
            incident.push(Vec::new());
            next
        };
        if incident[id].last() != Some(&qidx) {
            incident[id].push(qidx);
        }
        Ok(())
    }

    fn intern_term_bnodes<'a>(
        term: &'a Term,
        bnode_ids: &mut HashMap<&'a str, usize>,
        incident: &mut Vec<Vec<usize>>,
        qidx: usize,
        max_blank_nodes: usize,
    ) -> GitResult<()> {
        match term {
            Term::BlankNode(b) => {
                intern_bnode(b.as_str(), bnode_ids, incident, qidx, max_blank_nodes)
            }
            Term::Triple(tr) => {
                if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                    intern_bnode(b.as_str(), bnode_ids, incident, qidx, max_blank_nodes)?;
                }
                intern_term_bnodes(&tr.object, bnode_ids, incident, qidx, max_blank_nodes)
            }
            _ => Ok(()),
        }
    }

    fn format_subject_first_degree(subj: &NamedOrBlankNode, me: &str) -> String {
        match subj {
            NamedOrBlankNode::NamedNode(n) => format!("<{}>", n.as_str()),
            NamedOrBlankNode::BlankNode(b) => {
                if b.as_str() == me {
                    "_:a".to_string()
                } else {
                    "_:z".to_string()
                }
            }
        }
    }

    fn format_term_first_degree(term: &Term, me: &str) -> String {
        match term {
            Term::BlankNode(b) => {
                if b.as_str() == me {
                    "_:a".to_string()
                } else {
                    "_:z".to_string()
                }
            }
            Term::Triple(tr) => {
                let s = format_subject_first_degree(&tr.subject, me);
                let o = format_term_first_degree(&tr.object, me);
                format!("<<( {s} <{}> {o} )>>", tr.predicate.as_str())
            }
            other => other.to_string(),
        }
    }

    fn collect_triple_neighbors(
        tr: &Triple,
        u: usize,
        bnode_ids: &HashMap<&str, usize>,
        is_ambiguous: &[bool],
        out: &mut Vec<(u8, usize)>,
    ) {
        if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
            let id = bnode_ids[b.as_str()];
            if id != u && is_ambiguous[id] {
                out.push((ROLE_OS, id));
            }
        }
        match &tr.object {
            Term::BlankNode(b) => {
                let id = bnode_ids[b.as_str()];
                if id != u && is_ambiguous[id] {
                    out.push((ROLE_OO, id));
                }
            }
            Term::Triple(inner) => {
                collect_triple_neighbors(inner, u, bnode_ids, is_ambiguous, out);
            }
            _ => {}
        }
    }

    for (qidx, q) in quads.iter().enumerate() {
        if let NamedOrBlankNode::BlankNode(b) = &q.subject {
            intern_bnode(
                b.as_str(),
                &mut bnode_ids,
                &mut incident,
                qidx,
                max_blank_nodes,
            )?;
        }
        intern_term_bnodes(
            &q.object,
            &mut bnode_ids,
            &mut incident,
            qidx,
            max_blank_nodes,
        )?;
        if let GraphName::BlankNode(b) = &q.graph_name {
            intern_bnode(
                b.as_str(),
                &mut bnode_ids,
                &mut incident,
                qidx,
                max_blank_nodes,
            )?;
        }
    }

    let n_bnodes = bnode_ids.len();
    if n_bnodes <= 1 {
        return Ok(());
    }

    // 1. Compute 1-hop signature per blank node (mirroring oxrdf's RDFC-1.0
    //    `hash_first_degree_quads`, including RDF 1.2 triple terms).
    let mut sigs: Vec<String> = Vec::with_capacity(n_bnodes);
    let mut labels_by_id: Vec<&str> = vec![""; n_bnodes];
    for (&label, &id) in &bnode_ids {
        labels_by_id[id] = label;
    }

    let mut sig_counts: HashMap<String, usize> = HashMap::new();
    for (id, qidxs) in incident.iter().enumerate() {
        let me = labels_by_id[id];
        let mut lines: Vec<String> = Vec::with_capacity(qidxs.len());
        for &qi in qidxs {
            let q = &quads[qi];
            let s = format_subject_first_degree(&q.subject, me);
            let o = format_term_first_degree(&q.object, me);
            let g = match &q.graph_name {
                GraphName::DefaultGraph => String::new(),
                GraphName::NamedNode(n) => format!(" <{}>", n.as_str()),
                GraphName::BlankNode(b) => {
                    if b.as_str() == me {
                        " _:a".to_string()
                    } else {
                        " _:z".to_string()
                    }
                }
            };
            lines.push(format!("{s} <{}> {o}{g} .", q.predicate.as_str()));
        }
        lines.sort_unstable();
        let mut h = Sha256Stream::new();
        for l in &lines {
            h.update(l.as_bytes());
            h.update(b"\n");
        }
        let sig = h.finalize_hex();
        *sig_counts.entry(sig.clone()).or_insert(0) += 1;
        sigs.push(sig);
    }

    let is_ambiguous: Vec<bool> = sigs.iter().map(|s| sig_counts[s] > 1).collect();
    if !is_ambiguous.iter().any(|&a| a) {
        return Ok(());
    }

    // 2. For every blank node, check the size of each indistinguishable ambiguous-neighbor
    //    bucket (matching oxrdf's `hash_related_blank_node` keys: `(position, predicate_if_not_g,
    //    related_1hop_sig)` without `u`'s role), and build the ambiguous-to-ambiguous
    //    adjacency graph for component sizing.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n_bnodes];
    let mut others: Vec<(u8, usize)> = Vec::new();
    for (u, qidxs) in incident.iter().enumerate() {
        let mut buckets: HashMap<(u8, &str, &str), HashSet<usize>> = HashMap::new();
        for &qi in qidxs {
            let q = &quads[qi];
            others.clear();

            if let NamedOrBlankNode::BlankNode(b) = &q.subject {
                let id = bnode_ids[b.as_str()];
                if id != u && is_ambiguous[id] {
                    others.push((ROLE_S, id));
                }
            }
            match &q.object {
                Term::BlankNode(b) => {
                    let id = bnode_ids[b.as_str()];
                    if id != u && is_ambiguous[id] {
                        others.push((ROLE_O, id));
                    }
                }
                Term::Triple(tr) => {
                    collect_triple_neighbors(tr, u, &bnode_ids, &is_ambiguous, &mut others);
                }
                _ => {}
            }
            if let GraphName::BlankNode(b) = &q.graph_name {
                let id = bnode_ids[b.as_str()];
                if id != u && is_ambiguous[id] {
                    others.push((ROLE_G, id));
                }
            }

            for &(v_role, v) in &others {
                if is_ambiguous[u] {
                    adj[u].push(v);
                }
                let pred_key = if v_role == ROLE_G {
                    ""
                } else {
                    q.predicate.as_str()
                };
                let set = buckets
                    .entry((v_role, pred_key, sigs[v].as_str()))
                    .or_default();
                set.insert(v);
                if set.len() > max_ambiguous_group {
                    return Err(GitError::Format(format!(
                        "RDFC-1.0 canonicalization budget exceeded: blank node has more than \
                         {max_ambiguous_group} indistinguishable adjacent blank nodes"
                    )));
                }
            }
        }
    }

    // 3. Cap connected component size among mutually adjacent ambiguous blank nodes.
    let max_component = max_ambiguous_group.saturating_mul(8).max(32);
    let mut visited = vec![false; n_bnodes];
    let mut queue = VecDeque::new();
    for start in 0..n_bnodes {
        if !is_ambiguous[start] || visited[start] || adj[start].is_empty() {
            continue;
        }
        visited[start] = true;
        queue.push_back(start);
        let mut comp_size = 0usize;
        while let Some(node) = queue.pop_front() {
            comp_size += 1;
            if comp_size > max_component {
                return Err(GitError::Format(format!(
                    "RDFC-1.0 canonicalization budget exceeded: ambiguous blank-node component \
                     exceeds {max_component} nodes"
                )));
            }
            for &next in &adj[node] {
                if !visited[next] {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
    }

    Ok(())
}

// ------------------------------------------------------------ serialization --------

/// Stream a single graph's already-RDFC-1.0-canonicalized quads as deterministic sorted
/// canonical N-Quads, byte-identical across all build feature combinations.
fn write_canonical_nquads(quads: &[Quad], out: &mut impl Write) -> GitResult<()> {
    let mut lines: Vec<String> = quads.iter().map(|q| format!("{q} .")).collect();
    lines.sort_unstable();
    lines.dedup();
    for line in &lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Stream a single graph's already-RDFC-1.0-canonicalized quads as deterministic sorted,
/// subject-grouped, diff-friendly Turtle with stable prefixes, byte-identical across all
/// build feature combinations.
fn write_canonical_turtle(
    quads: &[Quad],
    prefixes: &BTreeMap<String, String>,
    out: &mut impl Write,
) -> GitResult<()> {
    let compactor = PrefixCompactor::new(prefixes);
    let mut used_prefixes: BTreeSet<String> = BTreeSet::new();

    // Group by rendered subject -> rendered predicate (`a` first, then codepoint order) ->
    // sorted unique rendered objects.
    let mut subjects: BTreeMap<String, BTreeMap<PredicateKey, BTreeSet<String>>> = BTreeMap::new();
    for q in quads {
        let s = render_subject(&q.subject, &compactor, &mut used_prefixes);
        let p = render_predicate(&q.predicate, &compactor, &mut used_prefixes);
        let o = render_term(&q.object, &compactor, &mut used_prefixes);
        subjects
            .entry(s)
            .or_default()
            .entry(p)
            .or_default()
            .insert(o);
    }

    if !used_prefixes.is_empty() {
        for p in &used_prefixes {
            if let Some(ns) = prefixes.get(p) {
                writeln!(out, "PREFIX {p}: <{ns}>")?;
            }
        }
        out.write_all(b"\n")?;
    }

    let mut first_subject = true;
    for (subject, predicates) in &subjects {
        if !first_subject {
            out.write_all(b"\n")?;
        }
        first_subject = false;

        if predicates.len() == 1 {
            let (pred, objs) = predicates.iter().next().expect("non-empty");
            if objs.len() == 1 {
                let obj = objs.iter().next().expect("non-empty");
                writeln!(out, "{subject} {} {obj} .", pred.text)?;
                continue;
            }
        }

        writeln!(out, "{subject}")?;
        for (pred, objs) in predicates {
            if objs.len() == 1 {
                let obj = objs.iter().next().expect("non-empty");
                writeln!(out, "  {} {obj} ;", pred.text)?;
            } else {
                writeln!(out, "  {}", pred.text)?;
                let last_idx = objs.len() - 1;
                for (i, obj) in objs.iter().enumerate() {
                    if i < last_idx {
                        writeln!(out, "    {obj} ,")?;
                    } else {
                        writeln!(out, "    {obj} ;")?;
                    }
                }
            }
        }
        out.write_all(b".\n")?;
    }
    Ok(())
}

/// Predicate sorting key: `a` (`rdf:type`) sorts first, followed by codepoint order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PredicateKey {
    is_type: bool,
    text: String,
}

impl Ord for PredicateKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (!self.is_type)
            .cmp(&(!other.is_type))
            .then_with(|| self.text.cmp(&other.text))
    }
}

impl PartialOrd for PredicateKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct PrefixCompactor {
    /// Sorted by namespace length descending, then prefix label ascending, for deterministic
    /// longest-prefix compaction.
    entries: Vec<(String, String)>,
}

impl PrefixCompactor {
    fn new(prefixes: &BTreeMap<String, String>) -> Self {
        let mut entries: Vec<(String, String)> = prefixes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        Self { entries }
    }

    fn compact(&self, iri: &str, used: &mut BTreeSet<String>) -> String {
        for (prefix, ns) in &self.entries {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && is_safe_pn_local(local)
            {
                used.insert(prefix.clone());
                return format!("{prefix}:{local}");
            }
        }
        format!("<{iri}>")
    }
}

fn render_subject(
    s: &NamedOrBlankNode,
    compactor: &PrefixCompactor,
    used: &mut BTreeSet<String>,
) -> String {
    match s {
        NamedOrBlankNode::NamedNode(n) => compactor.compact(n.as_str(), used),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

fn render_predicate(
    p: &NamedNode,
    compactor: &PrefixCompactor,
    used: &mut BTreeSet<String>,
) -> PredicateKey {
    if p.as_str() == RDF_TYPE_IRI {
        PredicateKey {
            is_type: true,
            text: "a".to_string(),
        }
    } else {
        PredicateKey {
            is_type: false,
            text: compactor.compact(p.as_str(), used),
        }
    }
}

fn render_term(t: &Term, compactor: &PrefixCompactor, used: &mut BTreeSet<String>) -> String {
    match t {
        Term::NamedNode(n) => compactor.compact(n.as_str(), used),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => render_literal(l, compactor, used),
        Term::Triple(triple) => render_triple_term(triple, compactor, used),
    }
}

fn render_literal(l: &Literal, compactor: &PrefixCompactor, used: &mut BTreeSet<String>) -> String {
    if l.language().is_some() {
        return l.to_string();
    }
    let dt = l.datatype();
    let lex = l.value();
    if dt == xsd::STRING {
        return l.to_string();
    }
    if dt == xsd::BOOLEAN && (lex == "true" || lex == "false") {
        return lex.to_string();
    }
    if dt == xsd::INTEGER && is_canonical_turtle_integer(lex) {
        return lex.to_string();
    }
    let quoted = Literal::new_simple_literal(lex).to_string();
    let dt_rendered = compactor.compact(dt.as_str(), used);
    format!("{quoted}^^{dt_rendered}")
}

/// Strictly canonical Turtle integer literal whose lexical form round-trips identically
/// (`"0"` or optional `-` followed by `[1-9][0-9]*`). Non-canonical forms like `"+1"` or
/// `"01"` keep their explicit datatype annotation so lexical form is never altered.
fn is_canonical_turtle_integer(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    let rest = s.strip_prefix('-').unwrap_or(s);
    let bytes = rest.as_bytes();
    !bytes.is_empty()
        && (b'1'..=b'9').contains(&bytes[0])
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

fn render_triple_term(
    t: &Triple,
    compactor: &PrefixCompactor,
    used: &mut BTreeSet<String>,
) -> String {
    let s = render_subject(&t.subject, compactor, used);
    let p = render_predicate(&t.predicate, compactor, used);
    let o = render_term(&t.object, compactor, used);
    format!("<<( {s} {} {o} )>>", p.text)
}

/// Conservative Turtle `PN_PREFIX` validation (empty prefix `:` is allowed).
fn is_valid_prefix_label(p: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    let bytes = p.as_bytes();
    bytes[0].is_ascii_alphabetic()
        && *bytes.last().unwrap() != b'.'
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Conservative Turtle `PN_LOCAL` validation so compacted prefixed names are always valid
/// Turtle 1.2 without escaping ambiguities.
fn is_safe_pn_local(local: &str) -> bool {
    if local.is_empty() {
        return true;
    }
    let bytes = local.as_bytes();
    let first = bytes[0];
    let last = *bytes.last().unwrap();
    if !(first.is_ascii_alphanumeric() || first == b'_') {
        return false;
    }
    if last == b'.' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Bounded, CDT-aware check of whether two sets of quads are RDF-dataset isomorphic
/// (including blank-node structure across graphs and inside `cdt:List`/`cdt:Map` literals).
pub fn try_datasets_isomorphic(
    a: impl IntoIterator<Item = Quad>,
    b: impl IntoIterator<Item = Quad>,
) -> GitResult<bool> {
    let da = canonicalize_dataset_bounded(
        a,
        DEFAULT_MAX_QUADS,
        DEFAULT_MAX_BLANK_NODES,
        DEFAULT_MAX_AMBIGUOUS_GROUP,
    )?;
    let db = canonicalize_dataset_bounded(
        b,
        DEFAULT_MAX_QUADS,
        DEFAULT_MAX_BLANK_NODES,
        DEFAULT_MAX_AMBIGUOUS_GROUP,
    )?;
    Ok(da == db)
}

/// Check whether two sets of quads are RDF-dataset isomorphic (including blank-node
/// structure across graphs and inside `cdt:List`/`cdt:Map` literals) using bounded
/// dataset-wide RDFC-1.0 canonicalization. Returns `false` if canonicalization limits are
/// exceeded; use [`try_datasets_isomorphic`] to distinguish limit errors.
pub fn datasets_isomorphic(
    a: impl IntoIterator<Item = Quad>,
    b: impl IntoIterator<Item = Quad>,
) -> bool {
    try_datasets_isomorphic(a, b).unwrap_or(false)
}

/// Collect all quads from a Sparkles [`Dataset`].
pub fn collect_dataset_quads(dataset: &Dataset) -> GitResult<Vec<Quad>> {
    let snap = dataset.snapshot();
    let mut quads = Vec::new();
    snap.for_each_quad(|q| {
        if let Some(quad) = snap.quad_to_terms(q) {
            quads.push(quad);
        }
        Ok(())
    })?;
    Ok(quads)
}

/// Convenience helper for comparing a Sparkles [`Dataset`] against a [`CanonicalTree`].
pub fn dataset_matches_tree(dataset: &Dataset, tree: &CanonicalTree) -> GitResult<bool> {
    let actual = collect_dataset_quads(dataset)?;
    try_datasets_isomorphic(actual, tree.quads.iter().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_rejects_traversal_and_unsafe_forms() {
        for bad in [
            "",
            "/etc/passwd",
            "graphs/../secret.ttl",
            "../outside.ttl",
            "graphs/./default.ttl",
            "graphs/sub/nested.ttl",
            "graphs\\default.ttl",
            "graphs/default.ttl\0",
            "graphs/C:drive.ttl",
            "other/default.ttl",
        ] {
            let err = validate_graph_path(bad, CanonicalSyntax::Turtle).unwrap_err();
            assert!(
                matches!(err, GitError::UnsafePath { .. }),
                "expected UnsafePath for {bad:?}, got {err:?}"
            );
        }
        let wrong_ext = validate_graph_path("graphs/default.nq", CanonicalSyntax::Turtle);
        assert!(matches!(wrong_ext, Err(GitError::InvalidManifest(_))));
        assert!(validate_graph_path("graphs/default.ttl", CanonicalSyntax::Turtle).is_ok());
        assert!(validate_graph_path("graphs/default.nq", CanonicalSyntax::NQuads).is_ok());
    }

    #[test]
    fn manifest_refuses_unknown_format_version_before_field_parsing() {
        for v in [0u32, 2, 99] {
            let json = format!(r#"{{"format_version": {v}, "future_field": true}}"#);
            let err = parse_and_validate_manifest(json.as_bytes(), true).unwrap_err();
            assert!(
                matches!(
                    err,
                    GitError::UnsupportedFormatVersion {
                        found,
                        supported: 1,
                        ..
                    } if found == v
                ),
                "unexpected error for version {v}: {err:?}"
            );
        }
    }

    #[test]
    fn manifest_rejects_duplicate_json_keys() {
        let json = r#"{"format_version": 2, "format_version": 1}"#;
        let err = parse_and_validate_manifest(json.as_bytes(), true).unwrap_err();
        assert!(
            matches!(err, GitError::InvalidManifest(ref m) if m.contains("duplicate JSON key")),
            "expected duplicate JSON key error, got {err:?}"
        );
    }
}
