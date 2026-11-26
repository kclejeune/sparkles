//! Versioned canonical git serialization profile (`format_version = 1`).
//!
//! A canonical tree writes `.sparkles-git/manifest.json` alongside one file per non-empty
//! graph under `graphs/`, using either sorted, subject-grouped Turtle (`graphs/*.ttl`) or
//! sorted canonical N-Quads (`graphs/*.nq`). Blank nodes are canonicalized dataset-wide
//! with RDFC-1.0 (`SHA-256`) so isomorphic datasets produce byte-for-byte identical trees
//! and cross-graph blank-node identity round-trips losslessly.

use crate::Dataset;
use crate::commit::Receipt;
use crate::fsutil;
use crate::git::{GitError, GitResult};
use crate::store::Snapshot;
use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
use oxrdf::{
    BlankNode, Dataset as RdfDataset, GraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad,
    Term, Triple,
};
use oxrdfio::{RdfFormat, RdfParser};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
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
    /// Line width used by the Turtle formatter when `feature = "fmt"` is enabled.
    pub line_width: u16,
    /// Remove stale `.ttl`/`.nq` files in `graphs/` that are no longer present in the
    /// manifest (default: `true`). Unrelated files outside `graphs/` and `.sparkles-git/`
    /// are never touched.
    pub prune_stale_graphs: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            syntax: CanonicalSyntax::Turtle,
            extra_prefixes: BTreeMap::new(),
            line_width: 100,
            prune_stale_graphs: true,
        }
    }
}

/// Options for reading a canonical git tree.
#[derive(Clone, Debug)]
pub struct ReadOptions {
    /// Verify each graph file's SHA-256 digest and quad count against `manifest.json`
    /// (default: `true`).
    pub verify_checksums: bool,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            verify_checksums: true,
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
    let mut quads = Vec::new();
    snapshot.for_each_quad(|q| {
        if let Some(quad) = snapshot.quad_to_terms(q) {
            quads.push(quad);
        }
        Ok(())
    })?;
    write_git_tree(quads, prefixes, dir, options)
}

/// Write an arbitrary set of RDF quads and prefixes as a versioned canonical git tree under `dir`.
pub fn write_git_tree(
    quads: impl IntoIterator<Item = Quad>,
    prefixes: &BTreeMap<String, String>,
    dir: &Path,
    options: &WriteOptions,
) -> GitResult<CanonicalManifest> {
    // 1. Dataset-wide RDFC-1.0 canonicalization so blank-node labels are deterministic
    //    and cross-graph blank-node identity + blank-node graph names stay aligned.
    let mut rdf_dataset: RdfDataset = quads.into_iter().collect();
    rdf_dataset.canonicalize(RDFC10);

    // 2. Partition canonicalized quads deterministically by graph.
    let mut by_graph: BTreeMap<GraphKey, Vec<Quad>> = BTreeMap::new();
    for q in rdf_dataset.iter() {
        let owned = q.into_owned();
        let key = GraphKey::from_graph_name(&owned.graph_name);
        by_graph.entry(key).or_default().push(owned);
    }

    // 3. Build validated, sorted prefix table.
    let mut merged_prefixes = BTreeMap::new();
    for (k, v) in prefixes.iter().chain(options.extra_prefixes.iter()) {
        if is_valid_prefix_label(k) && oxiri::Iri::parse(v.as_str()).is_ok() {
            merged_prefixes.insert(k.clone(), v.clone());
        }
    }

    // 4. Prepare directories safely.
    ensure_dir_no_symlink(dir)?;
    let meta_dir = dir.join(METADATA_DIR);
    let graphs_dir = dir.join(GRAPHS_DIR);
    fs::create_dir_all(&meta_dir).map_err(|e| GitError::io(&meta_dir, e))?;
    fs::create_dir_all(&graphs_dir).map_err(|e| GitError::io(&graphs_dir, e))?;

    // 5. Assign collision-free relative filenames and serialize each graph.
    let mut used_paths: BTreeSet<String> = BTreeSet::new();
    let mut entries = Vec::with_capacity(by_graph.len());

    for (graph_key, graph_quads) in by_graph {
        let rel_path = assign_graph_path(&graph_key, options.syntax, &mut used_paths);
        validate_graph_path(&rel_path, options.syntax)?;
        let content = match options.syntax {
            CanonicalSyntax::Turtle => {
                serialize_canonical_turtle(&graph_quads, &merged_prefixes, options.line_width)?
            }
            CanonicalSyntax::NQuads => serialize_canonical_nquads(&graph_quads)?,
        };
        let bytes = content.as_bytes();
        let digest = sha256_hex(bytes);
        let full_path = resolve_safe_path(dir, &rel_path)?;
        fsutil::write_atomic(&full_path, bytes).map_err(|e| GitError::io(&full_path, e))?;

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
            quads: graph_quads.len() as u64,
            sha256: digest,
        });
    }

    // 6. Remove stale graph files in `graphs/` if requested.
    if options.prune_stale_graphs && graphs_dir.is_dir() {
        for entry in fs::read_dir(&graphs_dir).map_err(|e| GitError::io(&graphs_dir, e))? {
            let entry = entry.map_err(|e| GitError::io(&graphs_dir, e))?;
            let path = entry.path();
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            let rel = format!("{GRAPHS_DIR}/{name_str}");
            let is_graph_file = name_str.ends_with(".ttl") || name_str.ends_with(".nq");
            if is_graph_file && !used_paths.contains(&rel) {
                let meta = fs::symlink_metadata(&path).map_err(|e| GitError::io(&path, e))?;
                if meta.file_type().is_file() || meta.file_type().is_symlink() {
                    fs::remove_file(&path).map_err(|e| GitError::io(&path, e))?;
                }
            }
        }
    }

    // 7. Write versioned manifest atomically with deterministic formatting and trailing newline.
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
    let manifest_path = dir.join(MANIFEST_REL_PATH);
    fsutil::write_atomic(&manifest_path, &manifest_bytes)
        .map_err(|e| GitError::io(&manifest_path, e))?;

    Ok(manifest)
}

/// Read and strictly validate a canonical git serialization tree using default [`ReadOptions`].
pub fn read_git_tree(dir: &Path) -> GitResult<CanonicalTree> {
    read_git_tree_with(dir, &ReadOptions::default())
}

/// Read and strictly validate a canonical git serialization tree with explicit [`ReadOptions`].
pub fn read_git_tree_with(dir: &Path, options: &ReadOptions) -> GitResult<CanonicalTree> {
    let manifest_path = resolve_safe_path(dir, MANIFEST_REL_PATH)?;
    let raw_bytes = fs::read(&manifest_path).map_err(|e| GitError::io(&manifest_path, e))?;
    let manifest = parse_and_validate_manifest(&raw_bytes)?;

    let mut quads = Vec::new();
    for entry in &manifest.graphs {
        let expected_graph = entry.graph_name()?;
        let full_path = resolve_safe_path(dir, &entry.path)?;
        let file_bytes = fs::read(&full_path).map_err(|e| GitError::io(&full_path, e))?;

        if options.verify_checksums {
            let actual_sha = sha256_hex(&file_bytes);
            if actual_sha != entry.sha256 {
                return Err(GitError::InvalidManifest(format!(
                    "checksum mismatch for {}: expected {}, got {}",
                    entry.path, entry.sha256, actual_sha
                )));
            }
        }

        let mut graph_quad_count = 0u64;
        match manifest.syntax {
            CanonicalSyntax::Turtle => {
                let parser = RdfParser::from_format(RdfFormat::Turtle);
                for parsed in parser.for_slice(&file_bytes) {
                    let q = parsed.map_err(|e| GitError::RdfParse {
                        path: entry.path.clone(),
                        message: e.to_string(),
                    })?;
                    quads.push(Quad::new(
                        q.subject,
                        q.predicate,
                        q.object,
                        expected_graph.clone(),
                    ));
                    graph_quad_count += 1;
                }
            }
            CanonicalSyntax::NQuads => {
                let parser = RdfParser::from_format(RdfFormat::NQuads);
                for parsed in parser.for_slice(&file_bytes) {
                    let q = parsed.map_err(|e| GitError::RdfParse {
                        path: entry.path.clone(),
                        message: e.to_string(),
                    })?;
                    if q.graph_name != expected_graph {
                        return Err(GitError::RdfParse {
                            path: entry.path.clone(),
                            message: format!(
                                "quad graph {} does not match manifest graph {}",
                                q.graph_name, expected_graph
                            ),
                        });
                    }
                    quads.push(q);
                    graph_quad_count += 1;
                }
            }
        }

        if options.verify_checksums && graph_quad_count != entry.quads {
            return Err(GitError::InvalidManifest(format!(
                "quad count mismatch for {}: manifest declares {}, file parsed {}",
                entry.path, entry.quads, graph_quad_count
            )));
        }
    }

    let prefixes = manifest.prefixes.clone();
    Ok(CanonicalTree {
        manifest,
        prefixes,
        quads,
    })
}

/// Read a canonical git tree from `dir` and import it into `dataset` in one transaction.
pub fn import_git_tree(dataset: &Dataset, dir: &Path) -> GitResult<Receipt> {
    import_git_tree_with(
        dataset,
        dir,
        &ReadOptions::default(),
        crate::guard::WriteOptions::default(),
    )
}

/// Read a canonical git tree from `dir` and import it into `dataset` with explicit options.
pub fn import_git_tree_with(
    dataset: &Dataset,
    dir: &Path,
    read_options: &ReadOptions,
    write_options: crate::guard::WriteOptions,
) -> GitResult<Receipt> {
    let tree = read_git_tree_with(dir, read_options)?;
    if !tree.prefixes.is_empty() {
        dataset.store().add_prefixes(tree.prefixes)?;
    }
    let (_, receipt) = dataset.transaction_receipt_with(write_options, |tx| {
        for q in &tree.quads {
            tx.insert(q.as_ref())?;
        }
        Ok(())
    })?;
    Ok(receipt)
}

// ------------------------------------------------------------ validation & paths ----

/// Parse raw manifest JSON bytes, checking `format_version` BEFORE typed deserialization
/// so future versions always yield [`GitError::UnsupportedFormatVersion`].
pub(crate) fn parse_and_validate_manifest(bytes: &[u8]) -> GitResult<CanonicalManifest> {
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
    for entry in &manifest.graphs {
        let gname = entry.graph_name()?;
        let gkey = GraphKey::from_graph_name(&gname);
        if !seen_graphs.insert(gkey) {
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
    }

    Ok(manifest)
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

/// Ensure `dir` exists (creating if needed) and is not a symlink.
fn ensure_dir_no_symlink(dir: &Path) -> GitResult<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target root directory must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::UnsafePath {
                    path: dir.display().to_string(),
                    reason: "target root path is not a directory".into(),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|e| GitError::io(dir, e))?;
        }
        Err(e) => return Err(GitError::io(dir, e)),
    }
    Ok(())
}

/// Resolve `rel` inside `root`, verifying that no component beneath `root` is a symlink.
pub(crate) fn resolve_safe_path(root: &Path, rel: &str) -> GitResult<PathBuf> {
    validate_relative_path(rel)?;
    let mut cur = root.to_path_buf();
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Path does not exist yet (valid when resolving a destination to write).
            }
            Err(e) => return Err(GitError::io(&cur, e)),
        }
    }
    Ok(cur)
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

fn sanitize_slug(raw: &str) -> String {
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
    let tagged = sparkles_core::jobs::digest_hex(bytes);
    tagged
        .strip_prefix("sha256:")
        .unwrap_or(&tagged)
        .to_string()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ------------------------------------------------------------ serialization --------

/// Serialize a single graph's already-RDFC-1.0-canonicalized quads as deterministic sorted
/// N-Quads.
fn serialize_canonical_nquads(quads: &[Quad]) -> GitResult<String> {
    let mut lines: Vec<String> = quads.iter().map(|q| format!("{q} .")).collect();
    lines.sort_unstable();
    lines.dedup();
    let mut raw = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for line in &lines {
        raw.push_str(line);
        raw.push('\n');
    }

    #[cfg(feature = "fmt")]
    {
        // Reuse sparkles-fmt's line formatter with spill-capable sorting and safety checks
        // (keeping canonicalize=false because dataset-wide RDFC-1.0 has already assigned
        // cross-graph blank-node labels).
        let opts = sparkles_fmt::Options {
            sort: true,
            canonicalize: false,
            ..sparkles_fmt::Options::default()
        };
        let cfg = sparkles_fmt::LinesConfig::default();
        let mut out = Vec::with_capacity(raw.len());
        sparkles_fmt::format_lines(
            std::io::Cursor::new(raw.as_bytes()),
            &mut out,
            sparkles_fmt::Language::NQuads,
            &opts,
            &cfg,
        )
        .map_err(|e| GitError::Format(e.to_string()))?;
        String::from_utf8(out).map_err(|e| GitError::Format(e.to_string()))
    }

    #[cfg(not(feature = "fmt"))]
    {
        Ok(raw)
    }
}

/// Serialize a single graph's already-RDFC-1.0-canonicalized quads as deterministic sorted,
/// subject-grouped Turtle with stable prefixes.
fn serialize_canonical_turtle(
    quads: &[Quad],
    prefixes: &BTreeMap<String, String>,
    line_width: u16,
) -> GitResult<String> {
    let compactor = PrefixCompactor::new(prefixes);
    let mut used_prefixes: BTreeSet<String> = BTreeSet::new();

    // Group by rendered subject -> rendered predicate -> sorted unique rendered objects.
    // Subject and predicate keys use their rendered Turtle text so grouping and ordering
    // match `sparkles-fmt --sort` deterministically.
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

    let mut raw = String::new();
    if !used_prefixes.is_empty() {
        for p in &used_prefixes {
            if let Some(ns) = prefixes.get(p) {
                let _ = writeln!(raw, "PREFIX {p}: <{ns}>");
            }
        }
        raw.push('\n');
    }

    let mut first_subject = true;
    for (subject, predicates) in &subjects {
        if !first_subject {
            raw.push('\n');
        }
        first_subject = false;

        if predicates.len() == 1 {
            let (pred, objs) = predicates.iter().next().expect("non-empty");
            if objs.len() == 1 {
                let obj = objs.iter().next().expect("non-empty");
                let _ = writeln!(raw, "{subject} {} {obj} .", pred.text);
                continue;
            }
        }

        let _ = writeln!(raw, "{subject}");
        for (pred, objs) in predicates {
            if objs.len() == 1 {
                let obj = objs.iter().next().expect("non-empty");
                let _ = writeln!(raw, "  {} {obj} ;", pred.text);
            } else {
                let joined: Vec<&str> = objs.iter().map(String::as_str).collect();
                let _ = writeln!(raw, "  {} {} ;", pred.text, joined.join(", "));
            }
        }
        raw.push_str(".\n");
    }

    #[cfg(feature = "fmt")]
    {
        let opts = sparkles_fmt::Options {
            line_width,
            sort: true,
            prune_prefixes: true,
            directive_style: sparkles_fmt::DirectiveStyle::Sparql,
            type_shorthand: true,
            compact_iris: true,
            quote_style: sparkles_fmt::QuoteStyle::Double,
            turtle_layout: sparkles_fmt::TurtleLayout::Diff,
            ..sparkles_fmt::Options::default()
        };
        let formatted = sparkles_fmt::format(&raw, sparkles_fmt::Language::Turtle, &opts)
            .map_err(|e| GitError::Format(e.to_string()))?;
        Ok(formatted.text)
    }

    #[cfg(not(feature = "fmt"))]
    {
        let _ = line_width;
        Ok(raw)
    }
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
        Term::Literal(l) => l.to_string(),
        Term::Triple(triple) => render_triple_term(triple, compactor, used),
    }
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

/// Check whether two sets of quads are RDF-dataset isomorphic (including blank-node
/// structure across graphs) using dataset-wide RDFC-1.0 canonicalization.
pub fn datasets_isomorphic(
    a: impl IntoIterator<Item = Quad>,
    b: impl IntoIterator<Item = Quad>,
) -> bool {
    let mut da: RdfDataset = a.into_iter().collect();
    let mut db: RdfDataset = b.into_iter().collect();
    da.canonicalize(RDFC10);
    db.canonicalize(RDFC10);
    da == db
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
    Ok(datasets_isomorphic(actual, tree.quads.iter().cloned()))
}

/// Collect all quads of one graph from a [`Snapshot`].
#[allow(dead_code)]
pub(crate) fn snapshot_graph_quads(
    snapshot: &Snapshot,
    graph: GraphNameRef<'_>,
) -> GitResult<Vec<Quad>> {
    let target = graph.into_owned();
    let mut out = Vec::new();
    snapshot.for_each_quad(|q| {
        if let Some(quad) = snapshot.quad_to_terms(q)
            && quad.graph_name == target
        {
            out.push(quad);
        }
        Ok(())
    })?;
    Ok(out)
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
            let err = parse_and_validate_manifest(json.as_bytes()).unwrap_err();
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
}
