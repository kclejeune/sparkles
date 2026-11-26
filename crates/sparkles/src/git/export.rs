//! Git export of a Sparkles dataset branch (F25 Phase 2).
//!
//! Exports a branch's history into a standalone Git repository either commit-by-commit
//! ([`GitExportMode::Commits`]) or as a single squashed commit
//! ([`GitExportMode::Squash`]). Each Git commit carries the canonical tree produced by
//! [`crate::git::profile`], deterministic author/committer identity and UTC timestamps
//! derived from the Sparkles commits, RFC 822 commit trailers, and a versioned per-branch
//! bidirectional Sparkles-commit to Git-SHA mapping record (`format_version = 1`).

use crate::Dataset;
use crate::branch::{CommitRef, MAIN, NamedCommitRef};
use crate::commit::{CommitInfo, CommitKind, CommitRange, rfc3339_ms};
use crate::git::profile::{
    CanonicalSyntax, DEFAULT_MAX_MANIFEST_BYTES, GRAPHS_DIR, MANIFEST_FORMAT_VERSION,
    MANIFEST_REL_PATH, METADATA_DIR, WriteOptions, check_dest_file_not_symlink, effective_parent,
    ensure_child_dir_no_symlink, ensure_dir_no_symlink, normalize_dir_for_lstat,
    open_regular_file_no_symlink, read_bounded_regular_file, reject_duplicate_json_keys,
    resolve_safe_path, sanitize_slug, sha256_hex, validate_relative_path,
    verify_existing_dir_no_symlink, write_atomic_safe, write_snapshot_git_tree_tracked,
};
use crate::git::{GitError, GitResult};
use crate::history::{At, HistoryOptions};
use crate::store::{BranchCommit, Snapshot, Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use uuid::Uuid;

/// Supported format version of the bidirectional commit mapping record.
pub const MAPPING_FORMAT_VERSION: u32 = 1;
/// Stable profile identifier recorded in the mapping file.
pub const MAPPING_PROFILE_NAME: &str = "sparkles-git-mapping-v1";
/// Default maximum size of a bidirectional commit mapping file in bytes (16 MiB).
pub const DEFAULT_MAX_MAPPING_BYTES: u64 = DEFAULT_MAX_MANIFEST_BYTES;
/// Directory inside `.git/` where Sparkles export metadata and locks live.
pub const DEFAULT_SPARKLES_GIT_SUBDIR: &str = "sparkles";
/// Directory inside `.git/sparkles/` holding per-branch bidirectional mapping files.
pub const DEFAULT_MAPPINGS_SUBDIR: &str = "mappings";
/// Directory inside `.git/sparkles/` holding crash-recoverable pending publication journals.
const DEFAULT_PENDING_SUBDIR: &str = "pending";
/// Directory inside `.git/sparkles/` holding isolated staging directories and temp indexes.
const DEFAULT_STAGING_SUBDIR: &str = "staging";

const GIT_ENV_SCRUB: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_SUPER_PREFIX",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
];

const GIT_IN_PROGRESS_MARKERS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

/// How Sparkles commits are mapped onto Git commits during export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitExportMode {
    /// Replay each Sparkles commit in chronological order as its own Git commit.
    #[default]
    Commits,
    /// Write a single Git commit representing the target Sparkles commit (default: head).
    Squash,
}

impl GitExportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GitExportMode::Commits => "commits",
            GitExportMode::Squash => "squash",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "commits" | "per-commit" | "per_commit" | "replay" => Some(GitExportMode::Commits),
            "squash" | "squashed" => Some(GitExportMode::Squash),
            _ => None,
        }
    }
}

/// Options for [`export_branch_to_git`] and [`Dataset::export_git`].
#[derive(Clone, Debug)]
pub struct GitExportOptions {
    /// Sparkles branch to export (`None`: this dataset's own branch, typically `main`).
    pub branch: Option<String>,
    /// Target Git branch name (`None`: defaults to the Sparkles branch name).
    pub git_branch: Option<String>,
    /// Per-commit replay or squashed single-commit export.
    pub mode: GitExportMode,
    /// Canonical RDF syntax for per-graph files (`turtle` or `nquads`).
    pub syntax: CanonicalSyntax,
    /// Optional lowest Sparkles commit `seq` (inclusive) to export.
    pub from_seq: Option<u64>,
    /// Optional highest Sparkles commit `seq` (inclusive) to export (default: branch head).
    pub to_seq: Option<u64>,
    /// Include an unannotated empty root `Create` commit 0 even when later commits exist
    /// (default: `false`).
    pub include_empty_root: bool,
    /// Fallback Git author/committer name when a Sparkles commit records no author.
    pub default_author_name: String,
    /// Fallback Git author/committer email when a Sparkles commit has no email address.
    pub default_author_email: String,
    /// Optional commit message override for [`GitExportMode::Squash`].
    pub squash_message: Option<String>,
    /// Extra RFC 822 commit trailers (`(key, value)`) appended to every exported Git commit.
    /// Keys starting with `Sparkles-` (case-insensitive) are reserved and rejected.
    pub extra_trailers: Vec<(String, String)>,
    /// Override location for the versioned bidirectional mapping file. When `None`, stored
    /// per Git branch at `<git-dir>/sparkles/mappings/<slug>-<hash16>.json`.
    pub mapping_path: Option<PathBuf>,
    /// Additional prefixes merged into the canonical profile's prefix table.
    pub extra_prefixes: BTreeMap<String, String>,
    /// Maximum serialized size of the bidirectional commit mapping file in bytes.
    pub max_mapping_bytes: u64,
}

impl Default for GitExportOptions {
    fn default() -> Self {
        Self {
            branch: None,
            git_branch: None,
            mode: GitExportMode::Commits,
            syntax: CanonicalSyntax::Turtle,
            from_seq: None,
            to_seq: None,
            include_empty_root: false,
            default_author_name: "Sparkles".to_string(),
            default_author_email: "sparkles@localhost".to_string(),
            squash_message: None,
            extra_trailers: Vec::new(),
            mapping_path: None,
            extra_prefixes: BTreeMap::new(),
            max_mapping_bytes: DEFAULT_MAX_MAPPING_BYTES,
        }
    }
}

/// One bidirectional entry linking a Sparkles commit to a Git commit SHA.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitMappingEntry {
    pub sparkles_branch_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparkles_branch: Option<String>,
    pub sparkles_seq: u64,
    pub git_sha: String,
    pub mode: GitExportMode,
    pub timestamp_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squashed_from: Option<u64>,
}

/// Versioned bidirectional mapping between Sparkles commits and Git commit SHAs for one
/// exported Git branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitMapping {
    pub format_version: u32,
    pub profile: String,
    pub dataset_id: Uuid,
    pub branch: String,
    pub branch_id: Uuid,
    pub git_branch: String,
    pub syntax: CanonicalSyntax,
    pub entries: Vec<CommitMappingEntry>,
}

impl CommitMapping {
    /// Look up the Git commit SHA produced for `(branch_id, seq)`.
    pub fn git_sha(&self, branch_id: Uuid, seq: u64) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.sparkles_branch_id == branch_id && e.sparkles_seq == seq)
            .map(|e| e.git_sha.as_str())
    }

    /// Look up the Git commit SHA for a Sparkles commit `seq` regardless of branch UUID
    /// (convenient when querying a single exported linear branch).
    pub fn git_sha_for_seq(&self, seq: u64) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.sparkles_seq == seq)
            .map(|e| e.git_sha.as_str())
    }

    /// Look up the Sparkles commit entry corresponding to `git_sha`.
    pub fn sparkles_commit(&self, git_sha: &str) -> Option<&CommitMappingEntry> {
        let needle = git_sha.trim();
        self.entries
            .iter()
            .find(|e| e.git_sha.eq_ignore_ascii_case(needle))
    }

    /// Read and validate a versioned mapping file using [`DEFAULT_MAX_MAPPING_BYTES`],
    /// refusing unknown `format_version` with [`GitError::UnsupportedFormatVersion`],
    /// symlinks/non-regular files with [`GitError::UnsafePath`], and duplicate JSON keys
    /// with [`GitError::InvalidMapping`].
    pub fn read(path: &Path) -> GitResult<Self> {
        Self::read_with_max_bytes(path, DEFAULT_MAX_MAPPING_BYTES)
    }

    /// Read and validate a versioned mapping file with an explicit byte ceiling.
    pub fn read_with_max_bytes(path: &Path, max_bytes: u64) -> GitResult<Self> {
        let parent = effective_parent(path)?;
        verify_existing_dir_no_symlink(&parent, &parent.display().to_string())?;
        let file_name = path.file_name().ok_or_else(|| GitError::UnsafePath {
            path: path.display().to_string(),
            reason: "mapping path must name a file".into(),
        })?;
        let clean_path = parent.join(file_name);
        let label = clean_path.display().to_string();
        let bytes = read_bounded_regular_file(&clean_path, &label, max_bytes)?;
        parse_and_validate_mapping(&bytes, &label)
    }

    /// Serialize this mapping to deterministic JSON bytes and verify it fits within
    /// `max_bytes`.
    pub(crate) fn serialize_bounded(&self, max_bytes: u64) -> GitResult<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| GitError::InvalidMapping(format!("failed to serialize mapping: {e}")))?;
        bytes.push(b'\n');
        if bytes.len() as u64 > max_bytes {
            return Err(GitError::InvalidMapping(format!(
                "commit mapping ({} bytes) exceeds maximum mapping size ({max_bytes} bytes)",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Persist this mapping file atomically with deterministic formatting via an
    /// exclusively created temp file, enforcing [`DEFAULT_MAX_MAPPING_BYTES`] and rejecting
    /// symlinked parents or targets.
    pub fn write(&self, path: &Path) -> GitResult<()> {
        self.write_with_max_bytes(path, DEFAULT_MAX_MAPPING_BYTES)
    }

    /// Persist this mapping file atomically with an explicit byte ceiling.
    pub fn write_with_max_bytes(&self, path: &Path, max_bytes: u64) -> GitResult<()> {
        let bytes = self.serialize_bounded(max_bytes)?;
        let parent = effective_parent(path)?;
        ensure_dir_no_symlink(&parent)?;
        write_atomic_safe(path, &bytes)
    }
}

/// Crash-recoverable publication intent written before moving a Git ref or switching the
/// working tree checkout so a failure or interruption at any point can be cleanly
/// reconciled on retry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPublication {
    format_version: u32,
    mapping_path: PathBuf,
    ref_name: String,
    old_ref_sha: Option<String>,
    /// Commit that was checked out in HEAD / `.git/index` / worktree before this sync
    /// started (may differ from `old_ref_sha` when switching from another branch, or be
    /// `None` on an unborn checkout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prev_checkout_sha: Option<String>,
    /// Symbolic `HEAD` ref before this sync started (if `HEAD` was symbolic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prev_head_ref: Option<String>,
    new_commit_sha: String,
    /// Updated mapping to persist, or `None` for a journaled no-op branch switch where the
    /// ref and mapping did not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping: Option<CommitMapping>,
}

fn max_pending_journal_bytes(max_mapping_bytes: u64) -> u64 {
    max_mapping_bytes.saturating_add(64 * 1024)
}

fn serialize_pending_bounded(
    pending: &PendingPublication,
    max_mapping_bytes: u64,
) -> GitResult<Vec<u8>> {
    // Compact JSON guarantees an in-limit pretty-printed mapping never expands past the
    // journal ceiling due to nested indentation.
    let bytes = serde_json::to_vec(pending).map_err(|e| {
        GitError::InvalidMapping(format!("failed to serialize pending record: {e}"))
    })?;
    let max_pending = max_pending_journal_bytes(max_mapping_bytes);
    if bytes.len() as u64 > max_pending {
        return Err(GitError::InvalidMapping(format!(
            "pending publication journal ({} bytes) exceeds maximum journal size ({max_pending} bytes)",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Summary returned after exporting a Sparkles branch to Git.
#[derive(Clone, Debug, Serialize)]
pub struct GitExportReport {
    pub repo_dir: PathBuf,
    pub mapping_path: PathBuf,
    pub dataset_id: Uuid,
    pub branch: String,
    pub branch_id: Uuid,
    pub git_branch: String,
    pub mode: GitExportMode,
    pub syntax: CanonicalSyntax,
    /// Number of newly created Git commits in this run.
    pub commits_exported: usize,
    /// Number of already-mapped prefix commits skipped during incremental continuation.
    pub commits_skipped: usize,
    /// Resulting Git HEAD SHA of `git_branch`.
    pub head_git_sha: String,
    /// Full bidirectional commit mapping record.
    pub mapping: CommitMapping,
}

/// Return the default per-branch mapping path
/// (`<git-dir>/sparkles/mappings/<slug>-<hash16>.json`) for `git_branch` in `repo_dir`.
pub fn default_mapping_path(repo_dir: &Path, git_branch: &str) -> GitResult<PathBuf> {
    validate_git_branch_name(git_branch)?;
    let git_dir = resolve_exact_git_dir(repo_dir)?;
    let slug = sanitize_slug(git_branch);
    let hash = sha256_hex(git_branch.as_bytes());
    Ok(git_dir
        .join(DEFAULT_SPARKLES_GIT_SUBDIR)
        .join(DEFAULT_MAPPINGS_SUBDIR)
        .join(format!("{slug}-{}.json", &hash[..16])))
}

/// Parse and strictly validate a mapping JSON payload.
pub(crate) fn parse_and_validate_mapping(
    bytes: &[u8],
    file_label: &str,
) -> GitResult<CommitMapping> {
    reject_duplicate_json_keys(bytes)
        .map_err(|e| GitError::InvalidMapping(format!("malformed JSON: {e}")))?;

    let raw: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| GitError::InvalidMapping(format!("malformed JSON: {e}")))?;
    let obj = raw
        .as_object()
        .ok_or_else(|| GitError::InvalidMapping("mapping root must be a JSON object".into()))?;
    let version_val = obj
        .get("format_version")
        .ok_or_else(|| GitError::InvalidMapping("missing required field format_version".into()))?;
    let found_u64 = version_val.as_u64().ok_or_else(|| {
        GitError::InvalidMapping("format_version must be a non-negative integer".into())
    })?;
    let found = u32::try_from(found_u64).unwrap_or(u32::MAX);
    if found != MAPPING_FORMAT_VERSION {
        return Err(GitError::UnsupportedFormatVersion {
            file: file_label.to_string(),
            found,
            supported: MAPPING_FORMAT_VERSION,
        });
    }

    let mapping: CommitMapping = serde_json::from_value(raw)
        .map_err(|e| GitError::InvalidMapping(format!("invalid mapping fields: {e}")))?;
    if mapping.profile != MAPPING_PROFILE_NAME {
        return Err(GitError::InvalidMapping(format!(
            "unsupported mapping profile {:?}; expected {:?}",
            mapping.profile, MAPPING_PROFILE_NAME
        )));
    }
    validate_git_branch_name(&mapping.git_branch)?;

    let mut seen_shas: HashSet<String> = HashSet::new();
    let mut seen_commits: HashSet<(Uuid, u64)> = HashSet::new();
    for entry in &mapping.entries {
        if !is_git_sha(&entry.git_sha) {
            return Err(GitError::InvalidMapping(format!(
                "invalid git_sha {:?} for commit {}",
                entry.git_sha, entry.sparkles_seq
            )));
        }
        let lower_sha = entry.git_sha.to_ascii_lowercase();
        if !seen_shas.insert(lower_sha) {
            return Err(GitError::InvalidMapping(format!(
                "duplicate git_sha {:?} in mapping",
                entry.git_sha
            )));
        }
        if !seen_commits.insert((entry.sparkles_branch_id, entry.sparkles_seq)) {
            return Err(GitError::InvalidMapping(format!(
                "duplicate sparkles commit ({}, {}) in commit mapping",
                entry.sparkles_branch_id, entry.sparkles_seq
            )));
        }
    }

    Ok(mapping)
}

fn is_git_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

enum PinnedBranchStore<'a> {
    Borrowed(&'a Store),
    Branch(crate::store::BranchStore<'a>),
}

impl PinnedBranchStore<'_> {
    fn as_store(&self) -> &Store {
        match self {
            PinnedBranchStore::Borrowed(s) => s,
            PinnedBranchStore::Branch(b) => b,
        }
    }
}

/// Export a Sparkles dataset branch to a standalone Git repository at `repo_dir`.
pub fn export_branch_to_git(
    dataset: &Dataset,
    repo_dir: &Path,
    options: &GitExportOptions,
) -> GitResult<GitExportReport> {
    validate_extra_trailers(&options.extra_trailers)?;

    // Resolve the owning store and pin the exact branch store & UUID so branch deletion,
    // rename, or name reuse cannot silently redirect export to another branch.
    let owner_state = if dataset.store().is_branch() {
        let own_branch = dataset.store().branch_name();
        let own_id = dataset.store().branch_id();
        let state = dataset.owner_state().ok_or_else(|| {
            GitError::Store(crate::Error::invalid(
                "cannot export from a branch handle whose owning dataset is no longer open; \
                 call export_git on the owning dataset with GitExportOptions::branch",
            ))
        })?;
        // Require the handle's bound UUID to still be listed; never fall back to the old
        // branch name after deletion/recreation.
        let live_name = state
            .store
            .listed_branch_name(own_id, &crate::guard::WriteOptions::default())?
            .ok_or_else(|| {
                GitError::Store(crate::Error::invalid(format!(
                    "branch {own_branch:?} ({own_id}) no longer exists"
                )))
            })?;
        if let Some(requested) = options.branch.as_deref()
            && requested != live_name
        {
            return Err(GitError::Store(crate::Error::invalid(format!(
                "cannot export branch {requested:?} from a handle bound to branch {live_name:?}"
            ))));
        }
        Some((state, live_name))
    } else {
        None
    };

    let (store, pinned_store, target_branch): (&Store, PinnedBranchStore<'_>, String) =
        match &owner_state {
            Some((state, live_name)) => (
                &state.store,
                PinnedBranchStore::Borrowed(dataset.store()),
                live_name.clone(),
            ),
            None => {
                let b = options
                    .branch
                    .clone()
                    .unwrap_or_else(|| dataset.store().branch_name());
                let pinned = match dataset.store().branch(&b) {
                    Ok(bs) => PinnedBranchStore::Branch(bs),
                    Err(_) if b == MAIN => PinnedBranchStore::Borrowed(dataset.store()),
                    Err(e) => return Err(GitError::Store(e)),
                };
                (dataset.store(), pinned, b)
            }
        };

    let git_branch = options
        .git_branch
        .clone()
        .unwrap_or_else(|| target_branch.clone());
    validate_git_branch_name(&git_branch)?;

    // Gather target branch identity, prefixes, and validated commit range in ascending order
    // strictly from the pinned branch store.
    let BranchHistory {
        dataset_id,
        branch_id,
        prefixes: branch_prefixes,
        mut commits,
        dropped_empty_root,
    } = collect_branch_history(store, &pinned_store, &target_branch, options)?;

    if commits.is_empty() {
        return Err(GitError::HistoryUnavailable(format!(
            "no commits matched export range on branch {target_branch:?}"
        )));
    }

    // Ensure `repo_dir` is a safe standalone non-bare Git repository root (never hijacking
    // an enclosing ancestor repo, linked worktree, bare repo, or symlink).
    let (clean_repo, git_dir) = ensure_standalone_git_repo(repo_dir, &git_branch)?;
    let canon_repo = fs::canonicalize(&clean_repo).map_err(|e| GitError::io(&clean_repo, e))?;

    // Serialize concurrent exports to the same repository.
    let _repo_lock = acquire_export_lock(&git_dir)?;

    // Refuse any in-progress Git merge/rebase/cherry-pick/revert/bisect before touching
    // pending recovery or refs.
    ensure_no_in_progress_git_op(&git_dir)?;

    let ref_name = format!("refs/heads/{git_branch}");
    let pending_path = pending_publication_path(&git_dir, &git_branch)?;

    let mapping_path = match &options.mapping_path {
        Some(p) => prepare_mapping_path_safe(p)?,
        None => {
            let sparkles_dir = ensure_child_dir_no_symlink(&git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
            let mappings_dir = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_MAPPINGS_SUBDIR)?;
            let slug = sanitize_slug(&git_branch);
            let hash = sha256_hex(git_branch.as_bytes());
            mappings_dir.join(format!("{slug}-{}.json", &hash[..16]))
        }
    };
    validate_mapping_destination_no_overlap(&mapping_path, &canon_repo, &git_dir, &pending_path)?;

    // Refuse if another branch has an unreconciled interrupted publication pending.
    ensure_no_other_pending_publications(&git_dir, &pending_path)?;

    // Reconcile any interrupted previous publication on this branch before checking clean
    // repository state, so a failure on either side of `git update-ref` or mid-checkout
    // recovers cleanly without manual resets or duplicate commits.
    reconcile_pending_publication(
        &clean_repo,
        &git_dir,
        &pending_path,
        &mapping_path,
        &ref_name,
        &git_branch,
        dataset_id,
        branch_id,
        options.syntax,
        options.max_mapping_bytes,
    )?;

    // Refuse if the Git index has pre-existing staged changes or dirty tracked files so we
    // never clobber local work.
    ensure_clean_git_state(&clean_repo, &git_dir)?;

    #[cfg(test)]
    test_hooks::after_clean_check(&clean_repo);

    // Inspect target Git branch tip and existing mapping BEFORE mutating anything.
    let branch_head = git_rev_parse_opt(&clean_repo, &ref_name)?;
    let mapping_exists = match fs::symlink_metadata(&mapping_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: mapping_path.display().to_string(),
                    reason: "mapping path must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: mapping_path.display().to_string(),
                    reason: "mapping path must be a regular file".into(),
                });
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(GitError::io(&mapping_path, e)),
    };

    let (mut entries, mut skipped, squash_already_up_to_date) = validate_continuation(
        &clean_repo,
        &ref_name,
        branch_head.as_deref(),
        &mapping_path,
        mapping_exists,
        dataset_id,
        branch_id,
        &git_branch,
        options,
        &mut commits,
        dropped_empty_root,
    )?;

    // Populate change-log metadata (author, message) ONLY for commits that will actually be
    // exported. Propagate segment/record read failures BEFORE publishing any Git commit so
    // damaged change logs never silently erase branch authors.
    match options.mode {
        GitExportMode::Commits => {
            if skipped < commits.len() {
                populate_changelog_metadata(store, &mut commits[skipped..])?;
            }
        }
        GitExportMode::Squash => {
            if !squash_already_up_to_date {
                let last_idx = commits.len() - 1;
                populate_changelog_metadata(store, &mut commits[last_idx..])?;
            }
        }
    }

    let write_opts = WriteOptions {
        syntax: options.syntax,
        extra_prefixes: options.extra_prefixes.clone(),
        prune_stale_graphs: true,
        ..WriteOptions::default()
    };

    let mut current_ref_sha = branch_head;
    let mut exported_count = 0usize;

    match options.mode {
        GitExportMode::Commits => {
            for item in commits.iter().skip(skipped) {
                let snap = snapshot_for_commit(store, &pinned_store, &target_branch, item)?;
                let message = build_commit_message(
                    dataset_id,
                    &target_branch,
                    branch_id,
                    item,
                    GitExportMode::Commits,
                    None,
                    None,
                    &options.extra_trailers,
                );
                let (author_name, author_email) = resolve_author(
                    item.author.as_deref(),
                    &options.default_author_name,
                    &options.default_author_email,
                );

                let (new_sha, entry) = build_and_publish_commit(
                    &clean_repo,
                    &canon_repo,
                    &git_dir,
                    &ref_name,
                    &git_branch,
                    current_ref_sha.as_deref(),
                    snap.as_ref(),
                    &branch_prefixes,
                    &write_opts,
                    &message,
                    &author_name,
                    &author_email,
                    item.commit.timestamp_ms,
                    CommitMappingEntry {
                        sparkles_branch_id: item.branch_id,
                        sparkles_branch: item.branch.clone(),
                        sparkles_seq: item.commit.seq,
                        git_sha: String::new(),
                        mode: GitExportMode::Commits,
                        timestamp_ms: item.commit.timestamp_ms,
                        squashed_from: None,
                    },
                    &entries,
                    dataset_id,
                    &target_branch,
                    branch_id,
                    options.syntax,
                    options.max_mapping_bytes,
                    &mapping_path,
                    &pending_path,
                )?;
                entries.push(entry);
                current_ref_sha = Some(new_sha);
                exported_count += 1;
            }
        }
        GitExportMode::Squash => {
            if !squash_already_up_to_date {
                let head_item = commits.last().expect("commits non-empty");
                let first_seq = commits.first().expect("commits non-empty").commit.seq;
                let squashed_from = (commits.len() > 1).then_some(first_seq);

                let snap = snapshot_for_commit(store, &pinned_store, &target_branch, head_item)?;
                let message = build_commit_message(
                    dataset_id,
                    &target_branch,
                    branch_id,
                    head_item,
                    GitExportMode::Squash,
                    options.squash_message.as_deref(),
                    squashed_from,
                    &options.extra_trailers,
                );
                let (author_name, author_email) = resolve_author(
                    head_item.author.as_deref(),
                    &options.default_author_name,
                    &options.default_author_email,
                );

                let (_, entry) = build_and_publish_commit(
                    &clean_repo,
                    &canon_repo,
                    &git_dir,
                    &ref_name,
                    &git_branch,
                    current_ref_sha.as_deref(),
                    snap.as_ref(),
                    &branch_prefixes,
                    &write_opts,
                    &message,
                    &author_name,
                    &author_email,
                    head_item.commit.timestamp_ms,
                    CommitMappingEntry {
                        sparkles_branch_id: head_item.branch_id,
                        sparkles_branch: head_item.branch.clone(),
                        sparkles_seq: head_item.commit.seq,
                        git_sha: String::new(),
                        mode: GitExportMode::Squash,
                        timestamp_ms: head_item.commit.timestamp_ms,
                        squashed_from,
                    },
                    &entries,
                    dataset_id,
                    &target_branch,
                    branch_id,
                    options.syntax,
                    options.max_mapping_bytes,
                    &mapping_path,
                    &pending_path,
                )?;
                entries.push(entry);
                exported_count = 1;
                skipped = 0;
            }
        }
    }

    let head_git_sha = entries
        .last()
        .map(|e| e.git_sha.clone())
        .ok_or_else(|| GitError::HistoryUnavailable("no commits were exported".into()))?;

    // Even when up to date (0 new commits), ensure `HEAD`, `.git/index`, and the working
    // tree are checked out to `git_branch` byte-exact without `.gitattributes` filters,
    // using a crash-recoverable pending journal whenever a branch switch is needed.
    if exported_count == 0 {
        let prev_head = git_rev_parse_opt(&clean_repo, "HEAD")?;
        let prev_head_ref = git_symbolic_ref_opt(&clean_repo);
        let already_on_target = prev_head_ref.as_deref() == Some(ref_name.as_str())
            && prev_head
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(&head_git_sha));
        if !already_on_target {
            ensure_clean_git_state(&clean_repo, &git_dir)?;
            let pending = PendingPublication {
                format_version: MAPPING_FORMAT_VERSION,
                mapping_path: mapping_path.clone(),
                ref_name: ref_name.clone(),
                old_ref_sha: Some(head_git_sha.clone()),
                prev_checkout_sha: prev_head.clone(),
                prev_head_ref: prev_head_ref.clone(),
                new_commit_sha: head_git_sha.clone(),
                mapping: None,
            };
            let pending_bytes = serialize_pending_bounded(&pending, options.max_mapping_bytes)?;
            write_atomic_safe(&pending_path, &pending_bytes)?;
            #[cfg(test)]
            test_hooks::during_worktree_sync(&clean_repo)?;
            sync_worktree_to_commit_no_filters(
                &clean_repo,
                &git_dir,
                &git_branch,
                prev_head.as_deref(),
                prev_head_ref.as_deref(),
                &head_git_sha,
                SyncMode::Normal,
            )?;
            #[cfg(test)]
            test_hooks::before_remove_pending()?;
            remove_pending_file(&pending_path)?;
        }
    }

    let mapping = CommitMapping {
        format_version: MAPPING_FORMAT_VERSION,
        profile: MAPPING_PROFILE_NAME.to_string(),
        dataset_id,
        branch: target_branch.clone(),
        branch_id,
        git_branch: git_branch.clone(),
        syntax: options.syntax,
        entries,
    };

    Ok(GitExportReport {
        repo_dir: clean_repo,
        mapping_path,
        dataset_id,
        branch: target_branch,
        branch_id,
        git_branch,
        mode: options.mode,
        syntax: options.syntax,
        commits_exported: exported_count,
        commits_skipped: skipped,
        head_git_sha,
        mapping,
    })
}

/// Build a canonical tree in an isolated staging directory, assemble its Git tree object in
/// an isolated temporary `GIT_INDEX_FILE` (never reading or writing `.git/index`), create a
/// single-parent/orphan commit via `git commit-tree`, record a crash-recoverable pending
/// publication journal, advance the target ref via compare-and-swap `git update-ref`,
/// persist `mapping_path`, sync the working tree byte-exact without filters, and clear the
/// pending journal.
#[allow(clippy::too_many_arguments)]
fn build_and_publish_commit(
    clean_repo: &Path,
    canon_repo: &Path,
    git_dir: &Path,
    ref_name: &str,
    git_branch: &str,
    old_ref_sha: Option<&str>,
    snapshot: &Snapshot,
    branch_prefixes: &BTreeMap<String, String>,
    write_opts: &WriteOptions,
    message: &str,
    author_name: &str,
    author_email: &str,
    timestamp_ms: i64,
    mut entry_template: CommitMappingEntry,
    prior_entries: &[CommitMappingEntry],
    dataset_id: Uuid,
    target_branch: &str,
    branch_id: Uuid,
    syntax: CanonicalSyntax,
    max_mapping_bytes: u64,
    mapping_path: &Path,
    pending_path: &Path,
) -> GitResult<(String, CommitMappingEntry)> {
    // 1. Write the canonical profile tree inside an isolated temporary staging directory
    //    under canonical absolute `git_dir` so any canonicalization, format, or size error
    //    leaves `clean_repo` untouched and `git -C` works for relative `repo_dir` paths.
    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let staging_parent = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_STAGING_SUBDIR)?;
    let stage_dir = tempfile::Builder::new()
        .prefix("tree-")
        .tempdir_in(&staging_parent)
        .map_err(|e| GitError::io(&staging_parent, e))?;

    let (manifest, _) =
        write_snapshot_git_tree_tracked(snapshot, branch_prefixes, stage_dir.path(), write_opts)?;

    // 2. Preflight the repository working tree and verify `mapping_path` never overlaps
    //    any published or previously tracked worktree path BEFORE publishing any Git commit.
    let mut new_paths: Vec<&str> = Vec::with_capacity(1 + manifest.graphs.len());
    new_paths.push(MANIFEST_REL_PATH);
    for entry in &manifest.graphs {
        new_paths.push(entry.path.as_str());
    }
    let prev_head_sha = git_rev_parse_opt(clean_repo, "HEAD")?;
    let prev_head_ref = git_symbolic_ref_opt(clean_repo);
    let old_tracked = list_tracked_paths_in_commit(clean_repo, prev_head_sha.as_deref())?;
    for rel in new_paths
        .iter()
        .copied()
        .chain(old_tracked.iter().map(String::as_str))
    {
        if mapping_path == canon_repo.join(rel) {
            return Err(GitError::UnsafePath {
                path: mapping_path.display().to_string(),
                reason: format!("mapping path must not alias repository tracked path {rel:?}"),
            });
        }
    }
    preflight_worktree_collisions(clean_repo, &old_tracked, &new_paths)?;

    // 3. Hash every staged file directly into Git's object database without filters and
    //    assemble a Git tree object using an isolated temporary `GIT_INDEX_FILE`.
    let mut index_lines = String::new();
    for rel in &new_paths {
        let staged_file = stage_dir.path().join(rel);
        let staged_str = staged_file.to_string_lossy();
        let oid = run_git(
            clean_repo,
            &["hash-object", "-w", "--no-filters", "--", &staged_str],
            &[],
        )?;
        let oid = oid.trim();
        index_lines.push_str("100644 ");
        index_lines.push_str(oid);
        index_lines.push('\t');
        index_lines.push_str(rel);
        index_lines.push('\n');
    }

    let tmp_index = tempfile::Builder::new()
        .prefix("index-")
        .tempfile_in(&staging_parent)
        .map_err(|e| GitError::io(&staging_parent, e))?;
    let tmp_index_path = tmp_index.into_temp_path();
    // Remove the empty placeholder so Git creates a fresh index file cleanly.
    let _ = fs::remove_file(&tmp_index_path);
    let tmp_index_str = tmp_index_path.to_string_lossy().into_owned();
    let idx_env = [("GIT_INDEX_FILE", tmp_index_str.as_str())];

    run_git_with_stdin(
        clean_repo,
        &["update-index", "--add", "--index-info"],
        &idx_env,
        index_lines.as_bytes(),
    )?;
    let tree_oid = run_git(clean_repo, &["write-tree"], &idx_env)?;
    let tree_oid = tree_oid.trim().to_string();
    drop(tmp_index_path);

    // 4. Create the exact commit object with `git commit-tree` (never consulting
    //    `.git/index`, hooks, `MERGE_HEAD`, or message cleanup).
    let secs = timestamp_ms.div_euclid(1000).max(0);
    let git_date = format!("{secs} +0000");
    let commit_envs = [
        ("GIT_AUTHOR_NAME", author_name),
        ("GIT_AUTHOR_EMAIL", author_email),
        ("GIT_AUTHOR_DATE", git_date.as_str()),
        ("GIT_COMMITTER_NAME", author_name),
        ("GIT_COMMITTER_EMAIL", author_email),
        ("GIT_COMMITTER_DATE", git_date.as_str()),
    ];
    let mut commit_args: Vec<&str> = vec!["commit-tree", &tree_oid];
    if let Some(parent) = old_ref_sha {
        commit_args.push("-p");
        commit_args.push(parent);
    }
    let new_commit_sha =
        run_git_with_stdin(clean_repo, &commit_args, &commit_envs, message.as_bytes())?
            .trim()
            .to_string();

    entry_template.git_sha = new_commit_sha.clone();
    let mut next_entries = prior_entries.to_vec();
    next_entries.push(entry_template.clone());
    let next_mapping = CommitMapping {
        format_version: MAPPING_FORMAT_VERSION,
        profile: MAPPING_PROFILE_NAME.to_string(),
        dataset_id,
        branch: target_branch.to_string(),
        branch_id,
        git_branch: git_branch.to_string(),
        syntax,
        entries: next_entries,
    };

    // 5. Re-check repository clean state right before publication so any concurrent `git add`
    //    or tracked-file edit made during canonicalization aborts cleanly without moving the
    //    ref or clobbering local work, and size-check both the mapping and pending journal.
    ensure_clean_git_state(clean_repo, git_dir)?;
    let next_mapping_bytes = next_mapping.serialize_bounded(max_mapping_bytes)?;

    // 6. Persist the crash-recoverable pending publication record BEFORE moving the ref.
    let pending = PendingPublication {
        format_version: MAPPING_FORMAT_VERSION,
        mapping_path: mapping_path.to_path_buf(),
        ref_name: ref_name.to_string(),
        old_ref_sha: old_ref_sha.map(str::to_string),
        prev_checkout_sha: prev_head_sha.clone(),
        prev_head_ref: prev_head_ref.clone(),
        new_commit_sha: new_commit_sha.clone(),
        mapping: Some(next_mapping),
    };
    let pending_bytes = serialize_pending_bounded(&pending, max_mapping_bytes)?;
    write_atomic_safe(pending_path, &pending_bytes)?;

    #[cfg(test)]
    test_hooks::before_update_ref()?;

    // 7. Compare-and-swap ref update: fails atomically if another writer moved the ref.
    let expected_old = old_ref_sha.unwrap_or("");
    if let Err(err) = run_git(
        clean_repo,
        &["update-ref", ref_name, &new_commit_sha, expected_old],
        &[],
    ) {
        // Ref did not advance; clean up pending journal if still unchanged.
        if git_rev_parse_opt(clean_repo, ref_name)?.as_deref() == old_ref_sha {
            let _ = remove_pending_file(pending_path);
        }
        return Err(err);
    }

    // 8. Persist the mapping file atomically. If this fails, the pending journal remains on
    //    disk so the next retry reconciles cleanly without duplicating the commit.
    #[cfg(test)]
    test_hooks::during_mapping_write()?;
    write_atomic_safe(mapping_path, &next_mapping_bytes)?;

    // 9. Sync `HEAD`, `.git/index`, and the working tree byte-exact without `.gitattributes`
    //    filters under `.git/index.lock`, then remove the pending journal.
    #[cfg(test)]
    test_hooks::during_worktree_sync(clean_repo)?;
    sync_worktree_to_commit_no_filters(
        clean_repo,
        git_dir,
        git_branch,
        prev_head_sha.as_deref(),
        prev_head_ref.as_deref(),
        &new_commit_sha,
        SyncMode::Normal,
    )?;
    #[cfg(test)]
    test_hooks::before_remove_pending()?;
    remove_pending_file(pending_path)?;

    Ok((new_commit_sha, entry_template))
}

#[allow(clippy::too_many_arguments)]
fn reconcile_pending_publication(
    clean_repo: &Path,
    git_dir: &Path,
    pending_path: &Path,
    mapping_path: &Path,
    ref_name: &str,
    git_branch: &str,
    dataset_id: Uuid,
    branch_id: Uuid,
    syntax: CanonicalSyntax,
    max_mapping_bytes: u64,
) -> GitResult<()> {
    match fs::symlink_metadata(pending_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(GitError::io(pending_path, e)),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: pending_path.display().to_string(),
                    reason: "pending publication file must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: pending_path.display().to_string(),
                    reason: "pending publication path is not a regular file".into(),
                });
            }
        }
    }

    let label = pending_path.display().to_string();
    let max_pending = max_pending_journal_bytes(max_mapping_bytes);
    let raw = read_bounded_regular_file(pending_path, &label, max_pending)?;
    reject_duplicate_json_keys(&raw)
        .map_err(|e| GitError::InvalidMapping(format!("malformed pending record JSON: {e}")))?;
    let raw_val: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| GitError::InvalidMapping(format!("malformed pending record JSON: {e}")))?;
    let found_u64 = raw_val
        .as_object()
        .and_then(|o| o.get("format_version"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            GitError::InvalidMapping("pending publication record missing format_version".into())
        })?;
    let found = u32::try_from(found_u64).unwrap_or(u32::MAX);
    if found != MAPPING_FORMAT_VERSION {
        return Err(GitError::UnsupportedFormatVersion {
            file: label,
            found,
            supported: MAPPING_FORMAT_VERSION,
        });
    }
    let pending: PendingPublication = serde_json::from_value(raw_val).map_err(|e| {
        GitError::InvalidMapping(format!("invalid pending publication record: {e}"))
    })?;
    if pending.ref_name != ref_name || pending.mapping_path != mapping_path {
        return Err(GitError::InvalidMapping(format!(
            "unreconciled pending publication at {} does not match current export target",
            pending_path.display()
        )));
    }
    if let Some(ref pending_mapping) = pending.mapping
        && (pending_mapping.dataset_id != dataset_id
            || pending_mapping.branch_id != branch_id
            || pending_mapping.git_branch != git_branch
            || pending_mapping.syntax != syntax)
    {
        return Err(GitError::InvalidMapping(format!(
            "unreconciled pending publication at {} does not match current export target",
            pending_path.display()
        )));
    }

    let branch_head = git_rev_parse_opt(clean_repo, ref_name)?;
    let matches_old = match (branch_head.as_deref(), pending.old_ref_sha.as_deref()) {
        (None, None) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    };
    if pending.mapping.is_some() && matches_old {
        // Previous commit publication failed before advancing `ref_name`; clear the stale
        // pending file.
        remove_pending_file(pending_path)?;
        return Ok(());
    }

    let matches_new = branch_head
        .as_deref()
        .is_some_and(|h| h.eq_ignore_ascii_case(&pending.new_commit_sha));
    if !matches_new {
        return Err(GitError::InvalidMapping(format!(
            "git branch {git_branch:?} moved to {:?} while pending publication expected {:?} or {}",
            branch_head, pending.old_ref_sha, pending.new_commit_sha
        )));
    }

    // Complete mapping persistence if this pending record carries a new mapping entry,
    // verifying existing mapping consistency first.
    let mut need_mapping_write = false;
    if pending.mapping.is_some() {
        let mut already_persisted = false;
        match fs::symlink_metadata(mapping_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                    return Err(GitError::UnsafePath {
                        path: mapping_path.display().to_string(),
                        reason: "mapping path must be a non-symlink regular file".into(),
                    });
                }
                let existing = CommitMapping::read_with_max_bytes(mapping_path, max_mapping_bytes)?;
                let existing_tip = existing.entries.last().map(|e| e.git_sha.as_str());
                if existing_tip.is_some_and(|t| t.eq_ignore_ascii_case(&pending.new_commit_sha)) {
                    already_persisted = true;
                } else {
                    let tip_matches_old = match (existing_tip, pending.old_ref_sha.as_deref()) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                        _ => false,
                    };
                    if !tip_matches_old {
                        return Err(GitError::InvalidMapping(format!(
                            "existing mapping {} tip {:?} does not match pending recovery base {:?}",
                            mapping_path.display(),
                            existing_tip,
                            pending.old_ref_sha
                        )));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if pending.old_ref_sha.is_some() {
                    return Err(GitError::InvalidMapping(format!(
                        "mapping {} is missing during pending recovery from {:?}",
                        mapping_path.display(),
                        pending.old_ref_sha
                    )));
                }
            }
            Err(e) => return Err(GitError::io(mapping_path, e)),
        }
        need_mapping_write = !already_persisted;
    }

    // Replay or finish the checkout under `.git/index.lock` using the recorded prior
    // checkout identity, accepting files matching either the prior or target blob while
    // refusing any local edits, unmanaged collisions, or concurrent staging.
    sync_worktree_to_commit_no_filters(
        clean_repo,
        git_dir,
        git_branch,
        pending.prev_checkout_sha.as_deref(),
        pending.prev_head_ref.as_deref(),
        &pending.new_commit_sha,
        SyncMode::Recovery,
    )?;

    if need_mapping_write && let Some(ref pending_mapping) = pending.mapping {
        pending_mapping.write_with_max_bytes(mapping_path, max_mapping_bytes)?;
    }

    remove_pending_file(pending_path)?;
    Ok(())
}

fn ensure_no_other_pending_publications(git_dir: &Path, own_pending: &Path) -> GitResult<()> {
    let pending_dir = git_dir
        .join(DEFAULT_SPARKLES_GIT_SUBDIR)
        .join(DEFAULT_PENDING_SUBDIR);
    let rd = match fs::read_dir(&pending_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(GitError::io(&pending_dir, e)),
    };
    let own_name = own_pending.file_name();
    for entry in rd {
        let entry = entry.map_err(|e| GitError::io(&pending_dir, e))?;
        let name = entry.file_name();
        if Some(name.as_os_str()) == own_name {
            continue;
        }
        if name.to_string_lossy().ends_with(".json") {
            return Err(GitError::InvalidMapping(format!(
                "refusing export while another branch has an unreconciled pending publication ({}); \
                 retry that branch first",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn pending_publication_path(git_dir: &Path, git_branch: &str) -> GitResult<PathBuf> {
    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let pending_dir = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_PENDING_SUBDIR)?;
    let slug = sanitize_slug(git_branch);
    let hash = sha256_hex(git_branch.as_bytes());
    Ok(pending_dir.join(format!("{slug}-{}.json", &hash[..16])))
}

fn remove_pending_file(pending_path: &Path) -> GitResult<()> {
    match fs::remove_file(pending_path) {
        Ok(()) => {
            if let Some(parent) = pending_path.parent() {
                let _ = crate::fsutil::sync_dir(parent);
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(GitError::io(pending_path, e)),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_continuation(
    repo_dir: &Path,
    ref_name: &str,
    branch_head: Option<&str>,
    mapping_path: &Path,
    mapping_exists: bool,
    dataset_id: Uuid,
    branch_id: Uuid,
    git_branch: &str,
    options: &GitExportOptions,
    commits: &mut Vec<PlannedCommit>,
    dropped_empty_root: Option<PlannedCommit>,
) -> GitResult<(Vec<CommitMappingEntry>, usize, bool)> {
    if !mapping_exists {
        if let Some(head_sha) = branch_head {
            return Err(GitError::InvalidMapping(format!(
                "target git branch {git_branch:?} already has commits at {head_sha} with no \
                 matching Sparkles mapping at {}; refusing to append duplicate or unmapped history",
                mapping_path.display()
            )));
        }
        return Ok((Vec::new(), 0, false));
    }

    let existing = CommitMapping::read_with_max_bytes(mapping_path, options.max_mapping_bytes)?;
    if existing.dataset_id != dataset_id {
        return Err(GitError::InvalidMapping(format!(
            "mapping {} belongs to dataset {}, not {dataset_id}",
            mapping_path.display(),
            existing.dataset_id
        )));
    }
    if existing.branch_id != branch_id {
        return Err(GitError::InvalidMapping(format!(
            "mapping {} belongs to Sparkles branch {} ({}), not {branch_id}",
            mapping_path.display(),
            existing.branch,
            existing.branch_id
        )));
    }
    if existing.git_branch != git_branch {
        return Err(GitError::InvalidMapping(format!(
            "mapping {} belongs to git branch {:?}, not {git_branch:?}",
            mapping_path.display(),
            existing.git_branch
        )));
    }
    if existing.syntax != options.syntax {
        return Err(GitError::InvalidMapping(format!(
            "mapping {} uses syntax {}, cannot continue with syntax {}",
            mapping_path.display(),
            existing.syntax.as_str(),
            options.syntax.as_str()
        )));
    }

    let Some(last) = existing.entries.last() else {
        if let Some(head_sha) = branch_head {
            return Err(GitError::InvalidMapping(format!(
                "mapping {} is empty while git branch {git_branch:?} is at {head_sha}",
                mapping_path.display()
            )));
        }
        return Ok((Vec::new(), 0, false));
    };

    let Some(head_sha) = branch_head else {
        return Err(GitError::InvalidMapping(format!(
            "mapping {} expects git branch {git_branch:?} at {}, but the branch has no commits",
            mapping_path.display(),
            last.git_sha
        )));
    };
    if !last.git_sha.eq_ignore_ascii_case(head_sha) {
        return Err(GitError::InvalidMapping(format!(
            "git branch {git_branch:?} HEAD ({head_sha}) does not match last mapped commit ({}); \
             refusing to append on top of diverged or modified Git history",
            last.git_sha
        )));
    }

    // Verify every mapped SHA is present in exact single-parent chronological order on the
    // ref (no unexpected extra parents from merges).
    let lineage_raw = run_git(
        repo_dir,
        &[
            "rev-list",
            "--parents",
            "--first-parent",
            "--reverse",
            ref_name,
        ],
        &[],
    )?;
    let lineage_lines: Vec<Vec<&str>> = lineage_raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|l| l.split_whitespace().collect())
        .collect();
    if lineage_lines.len() != existing.entries.len() {
        return Err(GitError::InvalidMapping(format!(
            "git branch {git_branch:?} history length does not match mapping {}",
            mapping_path.display()
        )));
    }
    for (idx, (parts, entry)) in lineage_lines
        .iter()
        .zip(existing.entries.iter())
        .enumerate()
    {
        let Some(&actual_sha) = parts.first() else {
            return Err(GitError::InvalidMapping(format!(
                "git branch {git_branch:?} returned malformed rev-list entry"
            )));
        };
        if !actual_sha.eq_ignore_ascii_case(&entry.git_sha) {
            return Err(GitError::InvalidMapping(format!(
                "git branch {git_branch:?} first-parent history does not match mapping {}",
                mapping_path.display()
            )));
        }
        let expected_parents = usize::from(idx > 0);
        if parts.len().saturating_sub(1) != expected_parents {
            return Err(GitError::InvalidMapping(format!(
                "git branch {git_branch:?} commit {actual_sha} has unexpected parent count {}",
                parts.len().saturating_sub(1)
            )));
        }
    }

    match options.mode {
        GitExportMode::Commits => {
            if existing
                .entries
                .iter()
                .any(|e| e.mode != GitExportMode::Commits)
            {
                return Err(GitError::InvalidMapping(format!(
                    "mapping {} contains non-commits entries; cannot continue in commits mode",
                    mapping_path.display()
                )));
            }
            // If an earlier default export captured empty root commit 0 (when the store had
            // no other commits yet), preserve that mapped root on incremental default
            // continuation instead of refusing once commit 1 arrives.
            if options.from_seq.is_none()
                && let Some(root) = dropped_empty_root
                && existing.entries.first().is_some_and(|first| {
                    first.sparkles_branch_id == root.branch_id
                        && first.sparkles_seq == root.commit.seq
                })
            {
                commits.insert(0, root);
            }

            if commits.len() < existing.entries.len() {
                return Err(GitError::InvalidMapping(format!(
                    "requested commit range ({} commits) is shorter than already-mapped history \
                     ({} commits) on git branch {git_branch:?}",
                    commits.len(),
                    existing.entries.len()
                )));
            }
            for (idx, (mapped, planned)) in existing.entries.iter().zip(commits.iter()).enumerate()
            {
                if mapped.sparkles_branch_id != planned.branch_id
                    || mapped.sparkles_seq != planned.commit.seq
                {
                    return Err(GitError::InvalidMapping(format!(
                        "requested commit range diverges from mapped history at index {idx}: \
                         mapped ({}, {}), requested ({}, {})",
                        mapped.sparkles_branch_id,
                        mapped.sparkles_seq,
                        planned.branch_id,
                        planned.commit.seq
                    )));
                }
            }
            let skipped = existing.entries.len();
            Ok((existing.entries, skipped, false))
        }
        GitExportMode::Squash => {
            if existing
                .entries
                .iter()
                .any(|e| e.mode != GitExportMode::Squash)
            {
                return Err(GitError::InvalidMapping(format!(
                    "mapping {} was exported in commits mode; cannot mix squash mode on git \
                     branch {git_branch:?}",
                    mapping_path.display()
                )));
            }
            let head_item = commits.last().expect("commits non-empty");
            let first_seq = commits.first().expect("commits non-empty").commit.seq;
            let planned_squashed_from = (commits.len() > 1).then_some(first_seq);

            if head_item.commit.seq < last.sparkles_seq {
                return Err(GitError::InvalidMapping(format!(
                    "cannot squash-export older Sparkles commit {} onto git branch {git_branch:?} \
                     already mapped at commit {}",
                    head_item.commit.seq, last.sparkles_seq
                )));
            }
            if head_item.commit.seq == last.sparkles_seq {
                if last.sparkles_branch_id == head_item.branch_id
                    && last.squashed_from == planned_squashed_from
                {
                    return Ok((existing.entries, 1, true));
                }
                return Err(GitError::InvalidMapping(format!(
                    "squash range at Sparkles commit {} conflicts with already-mapped entry \
                     (branch {}, squashed_from {:?}) on git branch {git_branch:?}",
                    head_item.commit.seq, last.sparkles_branch_id, last.squashed_from
                )));
            }
            if existing.entries.iter().any(|e| {
                e.sparkles_branch_id == head_item.branch_id
                    && e.sparkles_seq == head_item.commit.seq
            }) {
                return Err(GitError::InvalidMapping(format!(
                    "Sparkles commit ({}, {}) is already present in squash mapping on git branch \
                     {git_branch:?}",
                    head_item.branch_id, head_item.commit.seq
                )));
            }
            Ok((existing.entries, 0, false))
        }
    }
}

// ------------------------------------------------------------ history gathering ----

struct PlannedCommit {
    commit: CommitInfo,
    branch: Option<String>,
    branch_id: Uuid,
    merged_from: Option<NamedCommitRef>,
    replayed_from: Option<NamedCommitRef>,
    message: Option<String>,
    digest_hex: Option<String>,
    author: Option<String>,
}

struct BranchHistory {
    dataset_id: Uuid,
    branch_id: Uuid,
    prefixes: BTreeMap<String, String>,
    commits: Vec<PlannedCommit>,
    dropped_empty_root: Option<PlannedCommit>,
}

fn collect_branch_history(
    owner_store: &Store,
    pinned_store: &PinnedBranchStore<'_>,
    target_branch: &str,
    options: &GitExportOptions,
) -> GitResult<BranchHistory> {
    if let (Some(from), Some(to)) = (options.from_seq, options.to_seq)
        && from > to
    {
        return Err(GitError::HistoryUnavailable(format!(
            "invalid commit range: from_seq ({from}) is greater than to_seq ({to})"
        )));
    }

    let dataset_id = owner_store.owner_dataset_id();
    let target_store = pinned_store.as_store();
    let branch_id = target_store.branch_id();
    let head_seq = target_store.head_commit().seq;
    let prefixes = target_store.prefixes();

    let raw_commits =
        match owner_store.branch_commits_pinned(target_store, CommitRange::Latest, usize::MAX) {
            Ok(mut list) => {
                list.reverse();
                list
            }
            Err(e) if target_branch == MAIN && branch_id == owner_store.branch_id() => {
                // Fallback for stores where branch operations are unsupported.
                let mut page = owner_store.commits(CommitRange::Latest, usize::MAX).commits;
                page.reverse();
                let _ = e;
                page.into_iter()
                    .map(|c| BranchCommit {
                        annotation: owner_store.annotation(c.seq),
                        commit: c,
                        branch: Some(MAIN.to_string()),
                        branch_id,
                        merged_from: None,
                        replayed_from: None,
                    })
                    .collect()
            }
            Err(e) => return Err(GitError::Store(e)),
        };

    if let Some(from) = options.from_seq
        && from > head_seq
    {
        return Err(GitError::HistoryUnavailable(format!(
            "requested from_seq ({from}) exceeds branch {target_branch:?} head ({head_seq})"
        )));
    }
    if let Some(to) = options.to_seq
        && to > head_seq
    {
        return Err(GitError::HistoryUnavailable(format!(
            "requested to_seq ({to}) exceeds branch {target_branch:?} head ({head_seq})"
        )));
    }

    let mut planned: Vec<PlannedCommit> = raw_commits
        .into_iter()
        .map(|bc| {
            let message = bc
                .annotation
                .as_ref()
                .and_then(|a| a.message.as_ref().map(|m| m.to_string()));
            let digest_hex = bc.annotation.as_ref().and_then(|a| a.digest_hex());
            PlannedCommit {
                commit: bc.commit,
                branch: bc.branch,
                branch_id: bc.branch_id,
                merged_from: bc.merged_from,
                replayed_from: bc.replayed_from,
                message,
                digest_hex,
                author: None,
            }
        })
        .collect();

    // Apply optional from_seq / to_seq bounds and validate that explicitly requested
    // endpoints were not silently dropped by catalog retention.
    if let Some(from) = options.from_seq {
        planned.retain(|p| p.commit.seq >= from);
        if planned.first().is_none_or(|p| p.commit.seq != from) {
            return Err(GitError::HistoryUnavailable(format!(
                "requested from_seq ({from}) on branch {target_branch:?} is outside retained \
                 commit metadata"
            )));
        }
    }
    if let Some(to) = options.to_seq {
        planned.retain(|p| p.commit.seq <= to);
        if planned.last().is_none_or(|p| p.commit.seq != to) {
            return Err(GitError::HistoryUnavailable(format!(
                "requested to_seq ({to}) on branch {target_branch:?} is outside retained \
                 commit metadata"
            )));
        }
    }

    // In per-commit mode, ensure the selected range has no missing catalog gaps.
    if options.mode == GitExportMode::Commits {
        for pair in planned.windows(2) {
            if pair[1].commit.seq != pair[0].commit.seq.saturating_add(1) {
                return Err(GitError::HistoryUnavailable(format!(
                    "commit {} on branch {target_branch:?} is missing from retained catalog \
                     metadata",
                    pair[0].commit.seq + 1
                )));
            }
        }
    }

    // Skip an unannotated empty root `Create` commit 0 when later commits exist, unless
    // explicitly requested via `include_empty_root` or `from_seq == Some(0)`. Keep a copy
    // so continuation can preserve commit 0 if an earlier export already mapped it.
    let dropped_empty_root = if !options.include_empty_root
        && options.from_seq != Some(0)
        && planned.len() > 1
        && planned[0].commit.seq == 0
        && planned[0].commit.kind == CommitKind::Create
        && planned[0].commit.quads == 0
        && planned[0].message.is_none()
    {
        Some(planned.remove(0))
    } else {
        None
    };

    Ok(BranchHistory {
        dataset_id,
        branch_id,
        prefixes,
        commits: planned,
        dropped_empty_root,
    })
}

/// Populate `(author, message)` from each owning branch store's change log for ONLY the
/// commits about to be exported. Flushes pending change-log records automatically, resolves
/// retired ancestor branches by UUID, and returns a typed error on corrupt or unreadable
/// change-log segments instead of silently substituting default authors.
fn populate_changelog_metadata(store: &Store, commits: &mut [PlannedCommit]) -> GitResult<()> {
    let mut seqs_by_branch: HashMap<Uuid, Vec<u64>> = HashMap::new();
    for item in commits.iter() {
        if item.commit.seq > 0 {
            seqs_by_branch
                .entry(item.branch_id)
                .or_default()
                .push(item.commit.seq);
        }
    }

    let mut metas_by_branch: HashMap<Uuid, HashMap<u64, crate::store::CommitChangeMeta>> =
        HashMap::new();
    for (bid, seqs) in seqs_by_branch {
        let map = if bid == store.branch_id() {
            store.commit_change_metas(&seqs)?
        } else {
            store.branch_commit_change_metas(bid, &seqs)?
        };
        metas_by_branch.insert(bid, map);
    }

    for item in commits.iter_mut() {
        if let Some((author, message)) = metas_by_branch
            .get(&item.branch_id)
            .and_then(|m| m.get(&item.commit.seq))
        {
            if item.author.is_none() {
                item.author = author.as_ref().map(|s| s.to_string());
            }
            if item
                .message
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                item.message = message.as_ref().map(|s| s.to_string());
            }
        }
    }
    Ok(())
}

fn snapshot_for_commit(
    owner_store: &Store,
    pinned_store: &PinnedBranchStore<'_>,
    target_branch: &str,
    item: &PlannedCommit,
) -> GitResult<Arc<Snapshot>> {
    let seq = item.commit.seq;
    let at = At::Commit(seq);
    let opts = HistoryOptions::default();
    let target = pinned_store.as_store();
    let res = match owner_store.snapshot_at_with_branch(target, &at, &opts) {
        Ok(v) => Ok(v),
        Err(_) if target_branch == MAIN && target.branch_id() == owner_store.branch_id() => {
            owner_store
                .snapshot_at(&at, &opts)
                .map(|(snap, resolved)| (snap, resolved, owner_store.branch_id()))
        }
        Err(e) => Err(e),
    };
    match res {
        Ok((snap, resolved, owner_bid)) => {
            if resolved.commit.seq != seq
                || (owner_bid != item.branch_id && owner_bid != target.branch_id())
            {
                return Err(GitError::HistoryUnavailable(format!(
                    "resolved snapshot ({owner_bid}, {}) does not match planned commit ({}, {seq}) \
                     on branch {target_branch:?}",
                    resolved.commit.seq, item.branch_id
                )));
            }
            Ok(snap)
        }
        Err(crate::Error::HistoryGone(gone)) => Err(GitError::HistoryUnavailable(format!(
            "commit {seq} on branch {target_branch:?} is no longer retained ({gone}); \
             use GitExportMode::Squash or widen history retention"
        ))),
        Err(crate::Error::HistoryUnsupported(msg)) => Err(GitError::HistoryUnavailable(format!(
            "commit {seq} on branch {target_branch:?} cannot be reconstructed ({msg}); \
             use GitExportMode::Squash or a persistent dataset with history retention"
        ))),
        Err(e) => Err(GitError::Store(e)),
    }
}

// ------------------------------------------------------------ message & author -----

#[allow(clippy::too_many_arguments)]
fn build_commit_message(
    dataset_id: Uuid,
    target_branch: &str,
    branch_id: Uuid,
    item: &PlannedCommit,
    mode: GitExportMode,
    squash_message: Option<&str>,
    squashed_from: Option<u64>,
    extra_trailers: &[(String, String)],
) -> String {
    let clean_msg = |raw: Option<&str>| -> Option<String> {
        raw.map(|s| s.replace('\0', ""))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let subject = match mode {
        GitExportMode::Squash => clean_msg(squash_message)
            .or_else(|| clean_msg(item.message.as_deref()))
            .unwrap_or_else(|| {
                format!(
                    "Sparkles export of {} at commit {}",
                    target_branch, item.commit.seq
                )
            }),
        GitExportMode::Commits => clean_msg(item.message.as_deref()).unwrap_or_else(|| {
            format!(
                "Sparkles commit {} ({})",
                item.commit.seq,
                item.commit.kind.name()
            )
        }),
    };

    let mut trailers: Vec<(String, String)> = vec![
        ("Sparkles-Dataset-Id".into(), dataset_id.to_string()),
        ("Sparkles-Branch".into(), target_branch.to_string()),
        ("Sparkles-Branch-Id".into(), branch_id.to_string()),
        ("Sparkles-Commit".into(), item.commit.seq.to_string()),
        (
            "Sparkles-Commit-Kind".into(),
            item.commit.kind.name().to_string(),
        ),
        (
            "Sparkles-Timestamp".into(),
            rfc3339_ms(item.commit.timestamp_ms),
        ),
        (
            "Sparkles-Format-Version".into(),
            MANIFEST_FORMAT_VERSION.to_string(),
        ),
        ("Sparkles-Export-Mode".into(), mode.as_str().to_string()),
    ];

    if item.branch_id != branch_id {
        let origin = format_named_commit(&NamedCommitRef {
            branch: item.branch.clone(),
            branch_id: item.branch_id,
            seq: item.commit.seq,
        });
        trailers.push(("Sparkles-Origin-Commit".into(), origin));
    }
    if let Some(digest) = &item.digest_hex {
        trailers.push(("Sparkles-Commit-Digest".into(), digest.clone()));
    }
    if let Some(merged) = &item.merged_from {
        trailers.push(("Sparkles-Merged-From".into(), format_named_commit(merged)));
    }
    if let Some(replayed) = &item.replayed_from {
        trailers.push((
            "Sparkles-Replayed-From".into(),
            format_named_commit(replayed),
        ));
    }
    if let Some(from) = squashed_from {
        trailers.push(("Sparkles-Squashed-From".into(), from.to_string()));
    }
    if let Some(raw_author) = &item.author {
        let clean = raw_author.replace(['\r', '\n', '\0'], " ");
        if !clean.trim().is_empty() {
            trailers.push(("Sparkles-Author".into(), clean.trim().to_string()));
        }
    }
    for (k, v) in extra_trailers {
        trailers.push((k.clone(), v.clone()));
    }

    let mut out = String::new();
    out.push_str(subject.trim_end());
    out.push_str("\n\n");
    for (k, v) in trailers {
        out.push_str(&k);
        out.push_str(": ");
        out.push_str(&v);
        out.push('\n');
    }
    out
}

fn format_named_commit(r: &NamedCommitRef) -> String {
    match &r.branch {
        Some(name) => format!("{name}@{} ({})", r.seq, r.branch_id),
        None => format!("{}@{}", r.branch_id, r.seq),
    }
}

/// Strip characters forbidden by Git's `ident.c` and ensure the resulting string is not
/// reduced to empty by Git's `strbuf_addstr_without_crud` trimming.
fn sanitize_git_ident_part(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\r' | '\n' | '\0') && !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    // Git's ident parser strips leading/trailing crud characters: `.`, `,`, `:`, `;`,
    // `<`, `>`, `"`, `\`, `'`, and whitespace. Ensure meaningful content remains.
    let without_crud = trimmed.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | ',' | ':' | ';' | '<' | '>' | '"' | '\\' | '\'')
    });
    if without_crud.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn resolve_author(raw: Option<&str>, default_name: &str, default_email: &str) -> (String, String) {
    let fallback_name = sanitize_git_ident_part(default_name).unwrap_or_else(|| "Sparkles".into());
    let fallback_email =
        sanitize_git_ident_part(default_email).unwrap_or_else(|| "sparkles@localhost".into());

    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return (fallback_name, fallback_email);
    };
    let clean = raw.replace(['\r', '\n', '\0'], " ");

    // `Display Name <email@example.com>`
    if let (Some(lt), Some(gt)) = (clean.find('<'), clean.rfind('>'))
        && lt < gt
    {
        let raw_name = clean[..lt].trim();
        let raw_email = clean[lt + 1..gt].trim();
        if let Some(email) = sanitize_git_ident_part(raw_email) {
            let name = sanitize_git_ident_part(raw_name).unwrap_or_else(|| email.clone());
            return (name, email);
        }
    }

    let Some(sanitized) = sanitize_git_ident_part(&clean) else {
        return (fallback_name, fallback_email);
    };

    // Bare email address
    if sanitized.contains('@') && !sanitized.contains(' ') {
        return (sanitized.clone(), sanitized);
    }

    // Plain principal/username
    let local: String = sanitized
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let local = local.trim_matches('.');
    let email = if local.is_empty() {
        fallback_email
    } else {
        format!("{local}@sparkles.local")
    };
    (sanitized, email)
}

fn validate_extra_trailers(trailers: &[(String, String)]) -> GitResult<()> {
    for (k, v) in trailers {
        if k.is_empty()
            || k.starts_with('-')
            || k.ends_with('-')
            || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(GitError::InvalidMapping(format!(
                "invalid trailer key {k:?}: expected ASCII letters, digits, and interior '-'"
            )));
        }
        if k.to_ascii_lowercase().starts_with("sparkles-") {
            return Err(GitError::InvalidMapping(format!(
                "trailer key {k:?} uses reserved prefix Sparkles-"
            )));
        }
        if v.contains('\n') || v.contains('\r') || v.contains('\0') {
            return Err(GitError::InvalidMapping(format!(
                "invalid trailer value for {k:?}: newlines and NUL are not allowed"
            )));
        }
    }
    Ok(())
}

/// Validate a Git branch name against `git-check-ref-format --branch` rules.
fn validate_git_branch_name(name: &str) -> GitResult<()> {
    let invalid = || GitError::UnsafePath {
        path: name.to_string(),
        reason: "invalid git branch name".into(),
    };
    if name.is_empty()
        || name == "@"
        || name == "HEAD"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err(invalid());
    }
    for segment in name.split('/') {
        if segment.is_empty()
            || segment.starts_with('.')
            || segment.ends_with('.')
            || segment.ends_with(".lock")
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Validate and prepare the custom mapping path and its parent directory BEFORE publishing
/// any Git commits, including bare relative paths such as `mapping.json`, returning one
/// canonical absolute destination path used consistently for all reads, journals, and writes.
fn prepare_mapping_path_safe(path: &Path) -> GitResult<PathBuf> {
    let parent = effective_parent(path)?;
    ensure_dir_no_symlink(&parent)?;
    let file_name = path.file_name().ok_or_else(|| GitError::UnsafePath {
        path: path.display().to_string(),
        reason: "mapping path must name a file".into(),
    })?;
    let clean_path = parent.join(file_name);
    check_dest_file_not_symlink(&clean_path)?;
    let canon_parent = fs::canonicalize(&parent).map_err(|e| GitError::io(&parent, e))?;
    Ok(canon_parent.join(file_name))
}

/// Refuse any custom mapping destination that aliases internal Git/pending metadata or
/// published canonical tree directories (`.sparkles-git/`, `graphs/`).
fn validate_mapping_destination_no_overlap(
    mapping_path: &Path,
    canon_repo: &Path,
    git_dir: &Path,
    pending_path: &Path,
) -> GitResult<()> {
    let sparkles_dir = git_dir.join(DEFAULT_SPARKLES_GIT_SUBDIR);
    let pending_dir = sparkles_dir.join(DEFAULT_PENDING_SUBDIR);
    let staging_dir = sparkles_dir.join(DEFAULT_STAGING_SUBDIR);
    let lock_path = sparkles_dir.join("export.lock");
    let forbidden_reason = if mapping_path == pending_path || mapping_path.starts_with(&pending_dir)
    {
        Some("mapping path must not alias Sparkles pending publication directory")
    } else if mapping_path.starts_with(&staging_dir) {
        Some("mapping path must not alias Sparkles staging directory")
    } else if mapping_path == lock_path {
        Some("mapping path must not alias Sparkles export lock")
    } else if mapping_path == git_dir.join("index")
        || mapping_path == git_dir.join("index.lock")
        || mapping_path == git_dir.join("HEAD")
        || mapping_path.starts_with(git_dir.join("refs"))
        || mapping_path.starts_with(git_dir.join("objects"))
    {
        Some("mapping path must not alias internal Git repository metadata")
    } else if mapping_path.starts_with(canon_repo.join(METADATA_DIR))
        || mapping_path.starts_with(canon_repo.join(GRAPHS_DIR))
    {
        Some(
            "mapping path must not lie inside published canonical tree paths (.sparkles-git/ or graphs/)",
        )
    } else {
        None
    };
    if let Some(reason) = forbidden_reason {
        return Err(GitError::UnsafePath {
            path: mapping_path.display().to_string(),
            reason: reason.into(),
        });
    }
    Ok(())
}

// ------------------------------------------------------------ git subprocesses -----

/// Build a `git` command targeting `repo_dir` with all repository/index override environment
/// variables scrubbed and deterministic config flags applied.
fn git_command(repo_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_ENV_SCRUB {
        cmd.env_remove(var);
    }
    cmd.arg("-C").arg(repo_dir).args([
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-c",
        "core.safecrlf=false",
        "-c",
        "core.quotepath=false",
        "-c",
        "core.attributesFile=/dev/null",
    ]);
    #[cfg(unix)]
    cmd.args(["-c", "core.hooksPath=/dev/null"]);
    cmd
}

/// Probe whether `clean_repo` itself is already a bare Git repository without walking into
/// enclosing parent repositories.
fn is_bare_git_repo_at(clean_repo: &Path) -> GitResult<bool> {
    let canon = fs::canonicalize(clean_repo).map_err(|e| GitError::io(clean_repo, e))?;
    let mut cmd = git_command(&canon);
    if let Some(parent) = canon.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let out = cmd
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .map_err(|e| GitError::GitCommand {
            args: vec!["rev-parse".into(), "--is-bare-repository".into()],
            status: None,
            stderr: e.to_string(),
        })?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
}

/// Ensure `repo_dir` is a standalone non-bare Git repository root (never an ancestor
/// repository, linked worktree, bare repository, or symlink) and return `(clean_repo, git_dir)`.
fn ensure_standalone_git_repo(repo_dir: &Path, git_branch: &str) -> GitResult<(PathBuf, PathBuf)> {
    let clean_repo = ensure_dir_no_symlink(repo_dir)?;
    let dot_git = clean_repo.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: dot_git.display().to_string(),
                    reason: "repository .git entry must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_dir() {
                return Err(GitError::InvalidMapping(format!(
                    "{} uses a .git gitfile (linked worktree or submodule); standalone Git \
                     repository required",
                    clean_repo.display()
                )));
            }
            let git_dir = resolve_exact_git_dir(&clean_repo)?;
            Ok((clean_repo, git_dir))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if is_bare_git_repo_at(&clean_repo)? {
                return Err(GitError::InvalidMapping(format!(
                    "{} is a bare Git repository; export requires a standalone non-bare \
                     repository",
                    clean_repo.display()
                )));
            }
            // Initialize a brand-new standalone repository directly inside `clean_repo`,
            // even if `clean_repo` is nested inside an enclosing Git working tree.
            run_git(&clean_repo, &["init", "-q", "-b", git_branch], &[])?;
            let git_dir = resolve_exact_git_dir(&clean_repo)?;
            Ok((clean_repo, git_dir))
        }
        Err(e) => Err(GitError::io(&dot_git, e)),
    }
}

/// Verify that `repo_dir` itself is the exact top-level working tree of a standalone
/// non-bare Git repository (not a linked worktree, submodule, bare repo, or symlink) and
/// return its resolved `.git` directory.
fn resolve_exact_git_dir(repo_dir: &Path) -> GitResult<PathBuf> {
    let clean_repo = verify_existing_dir_no_symlink(repo_dir, &repo_dir.display().to_string())?;
    let dot_git = clean_repo.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(GitError::UnsafePath {
                path: dot_git.display().to_string(),
                reason: "repository .git entry must not be a symlink".into(),
            });
        }
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(GitError::InvalidMapping(format!(
                "{} uses a .git gitfile (linked worktree or submodule); standalone Git \
                 repository required",
                clean_repo.display()
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(GitError::InvalidMapping(format!(
                "{} is not a Git repository root (no .git directory)",
                clean_repo.display()
            )));
        }
        Err(e) => return Err(GitError::io(&dot_git, e)),
    }

    let is_bare = run_git(&clean_repo, &["rev-parse", "--is-bare-repository"], &[])?;
    if is_bare.trim() != "false" {
        return Err(GitError::InvalidMapping(format!(
            "{} is a bare Git repository",
            clean_repo.display()
        )));
    }
    let inside = run_git(&clean_repo, &["rev-parse", "--is-inside-work-tree"], &[])?;
    if inside.trim() != "true" {
        return Err(GitError::InvalidMapping(format!(
            "{} is not a non-bare Git working tree",
            clean_repo.display()
        )));
    }
    let toplevel = run_git(&clean_repo, &["rev-parse", "--show-toplevel"], &[])?;
    let canon_top = fs::canonicalize(toplevel.trim()).map_err(|e| GitError::io(&clean_repo, e))?;
    let canon_repo = fs::canonicalize(&clean_repo).map_err(|e| GitError::io(&clean_repo, e))?;
    if canon_top != canon_repo {
        return Err(GitError::InvalidMapping(format!(
            "{} is not the Git repository root (top-level is {})",
            clean_repo.display(),
            canon_top.display()
        )));
    }

    let git_dir_raw = run_git(&clean_repo, &["rev-parse", "--git-dir"], &[])?;
    let git_dir = PathBuf::from(git_dir_raw.trim());
    let resolved = if git_dir.is_absolute() {
        git_dir
    } else {
        clean_repo.join(git_dir)
    };
    let resolved = verify_existing_dir_no_symlink(&resolved, &resolved.display().to_string())?;

    let common_raw = run_git(&clean_repo, &["rev-parse", "--git-common-dir"], &[])?;
    let common_dir = PathBuf::from(common_raw.trim());
    let resolved_common = if common_dir.is_absolute() {
        common_dir
    } else {
        clean_repo.join(common_dir)
    };
    let canon_git = fs::canonicalize(&resolved).map_err(|e| GitError::io(&resolved, e))?;
    let canon_common =
        fs::canonicalize(&resolved_common).map_err(|e| GitError::io(&resolved_common, e))?;
    if canon_git != canon_common {
        return Err(GitError::InvalidMapping(format!(
            "{} is a linked Git worktree (git-dir {} != git-common-dir {}); standalone Git \
             repository required",
            clean_repo.display(),
            canon_git.display(),
            canon_common.display()
        )));
    }
    Ok(canon_git)
}

struct ExportLock {
    _file: File,
}

fn acquire_export_lock(git_dir: &Path) -> GitResult<ExportLock> {
    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let lock_path = sparkles_dir.join("export.lock");
    match fs::symlink_metadata(&lock_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(GitError::UnsafePath {
                path: lock_path.display().to_string(),
                reason: "export lock file must not be a symlink".into(),
            });
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(GitError::io(&lock_path, e)),
    }
    let mut opts = OpenOptions::new();
    opts.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(&lock_path)
        .map_err(|e| GitError::io(&lock_path, e))?;
    file.lock().map_err(|e| GitError::io(&lock_path, e))?;
    Ok(ExportLock { _file: file })
}

fn ensure_no_in_progress_git_op(git_dir: &Path) -> GitResult<()> {
    for marker in GIT_IN_PROGRESS_MARKERS {
        let p = git_dir.join(marker);
        match fs::symlink_metadata(&p) {
            Ok(_) => {
                return Err(GitError::InvalidMapping(format!(
                    "refusing to export into a Git repository with an in-progress operation \
                     ({marker} present)"
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&p, e)),
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeBlobEntry {
    mode: String,
    oid: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncMode {
    Normal,
    Recovery,
}

/// Exclusive lock on `<git-dir>/index.lock` following Git's lockfile protocol so external
/// `git add`/`git rm`/`git checkout` cannot race with index inspection, worktree sync, or
/// the atomic swap of `.git/index`.
struct GitIndexLock {
    lock_path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl GitIndexLock {
    fn acquire(git_dir: &Path) -> GitResult<Self> {
        let lock_path = git_dir.join("index.lock");
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let file = opts.open(&lock_path).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                GitError::InvalidMapping(format!(
                    "refusing to modify Git index while {} exists (another Git operation is in \
                     progress)",
                    lock_path.display()
                ))
            } else {
                GitError::io(&lock_path, e)
            }
        })?;
        Ok(Self {
            lock_path,
            file: Some(file),
            committed: false,
        })
    }

    fn commit_index_bytes(mut self, git_dir: &Path, bytes: &[u8]) -> GitResult<()> {
        let mut f = self.file.take().expect("index lock file held");
        f.write_all(bytes)
            .map_err(|e| GitError::io(&self.lock_path, e))?;
        f.sync_all().map_err(|e| GitError::io(&self.lock_path, e))?;
        drop(f);
        let index_path = git_dir.join("index");
        check_dest_file_not_symlink(&index_path)?;
        fs::rename(&self.lock_path, &index_path).map_err(|e| GitError::io(&index_path, e))?;
        self.committed = true;
        crate::fsutil::sync_dir(git_dir).map_err(|e| GitError::io(git_dir, e))?;
        Ok(())
    }
}

impl Drop for GitIndexLock {
    fn drop(&mut self) {
        self.file.take();
        if !self.committed {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn git_symbolic_ref_opt(repo_dir: &Path) -> Option<String> {
    run_git(repo_dir, &["symbolic-ref", "-q", "HEAD"], &[])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_core_filemode(repo_dir: &Path) -> bool {
    #[cfg(not(unix))]
    {
        let _ = repo_dir;
        false
    }
    #[cfg(unix)]
    {
        match run_git(
            repo_dir,
            &["config", "--bool", "--get", "core.fileMode"],
            &[],
        ) {
            Ok(v) => v.trim() != "false",
            Err(_) => true,
        }
    }
}

fn read_commit_tree_entries(
    clean_repo: &Path,
    commit_sha: Option<&str>,
) -> GitResult<BTreeMap<String, TreeBlobEntry>> {
    let Some(sha) = commit_sha else {
        return Ok(BTreeMap::new());
    };
    let raw = run_git_bytes(clean_repo, &["ls-tree", "-r", "-z", sha], &[])?;
    let mut entries = BTreeMap::new();
    for rec in raw.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let Some(tab) = rec.iter().position(|&b| b == b'\t') else {
            return Err(GitError::InvalidMapping(
                "malformed git ls-tree output".into(),
            ));
        };
        let header = std::str::from_utf8(&rec[..tab])
            .map_err(|_| GitError::InvalidMapping("non-UTF-8 git ls-tree header".into()))?;
        let rel = std::str::from_utf8(&rec[tab + 1..])
            .map_err(|_| GitError::InvalidMapping("non-UTF-8 path in git tree".into()))?;
        let mut parts = header.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing mode in git ls-tree".into()))?;
        let _kind = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing kind in git ls-tree".into()))?;
        let oid = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing OID in git ls-tree".into()))?;
        entries.insert(
            rel.to_string(),
            TreeBlobEntry {
                mode: mode.to_string(),
                oid: oid.to_ascii_lowercase(),
            },
        );
    }
    Ok(entries)
}

fn read_index_entries(clean_repo: &Path) -> GitResult<BTreeMap<String, TreeBlobEntry>> {
    let raw = run_git_bytes(clean_repo, &["ls-files", "--stage", "-z"], &[])?;
    let mut entries = BTreeMap::new();
    for rec in raw.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let Some(tab) = rec.iter().position(|&b| b == b'\t') else {
            return Err(GitError::InvalidMapping(
                "malformed git ls-files --stage output".into(),
            ));
        };
        let header = std::str::from_utf8(&rec[..tab])
            .map_err(|_| GitError::InvalidMapping("non-UTF-8 git ls-files header".into()))?;
        let rel = std::str::from_utf8(&rec[tab + 1..])
            .map_err(|_| GitError::InvalidMapping("non-UTF-8 path in git index".into()))?;
        let mut parts = header.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing mode in git index".into()))?;
        let oid = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing OID in git index".into()))?;
        let stage = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing stage in git index".into()))?;
        if stage != "0" {
            return Err(GitError::InvalidMapping(format!(
                "refusing to export into a Git repository with unmerged index entry {rel:?}"
            )));
        }
        entries.insert(
            rel.to_string(),
            TreeBlobEntry {
                mode: mode.to_string(),
                oid: oid.to_ascii_lowercase(),
            },
        );
    }
    Ok(entries)
}

fn git_cat_file_batch(clean_repo: &Path, oids: &[&str]) -> GitResult<HashMap<String, Vec<u8>>> {
    let mut unique: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for oid in oids {
        let lower = oid.to_ascii_lowercase();
        if seen.insert(lower.clone()) {
            unique.push(lower);
        }
    }
    if unique.is_empty() {
        return Ok(HashMap::new());
    }
    let mut stdin = String::new();
    for oid in &unique {
        stdin.push_str(oid);
        stdin.push('\n');
    }
    let raw =
        run_git_bytes_with_stdin(clean_repo, &["cat-file", "--batch"], &[], stdin.as_bytes())?;
    let mut out = HashMap::with_capacity(unique.len());
    let mut cursor = 0usize;
    for req_oid in unique {
        let Some(nl_rel) = raw[cursor..].iter().position(|&b| b == b'\n') else {
            return Err(GitError::InvalidMapping(format!(
                "truncated git cat-file --batch header for {req_oid}"
            )));
        };
        let header = std::str::from_utf8(&raw[cursor..cursor + nl_rel])
            .map_err(|_| GitError::InvalidMapping("non-UTF-8 git cat-file header".into()))?;
        cursor += nl_rel + 1;
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() != 3 || parts[1] != "blob" {
            return Err(GitError::InvalidMapping(format!(
                "unexpected git cat-file --batch entry for {req_oid}: {header:?}"
            )));
        }
        let size: usize = parts[2].parse().map_err(|_| {
            GitError::InvalidMapping(format!(
                "invalid git cat-file --batch size for {req_oid}: {:?}",
                parts[2]
            ))
        })?;
        let end = cursor.checked_add(size).ok_or_else(|| {
            GitError::InvalidMapping(format!("git cat-file --batch size overflow for {req_oid}"))
        })?;
        if end > raw.len() {
            return Err(GitError::InvalidMapping(format!(
                "truncated git cat-file --batch body for {req_oid}"
            )));
        }
        out.insert(req_oid, raw[cursor..end].to_vec());
        cursor = end;
        if raw.get(cursor) != Some(&b'\n') {
            return Err(GitError::InvalidMapping(
                "missing trailing newline in git cat-file --batch output".into(),
            ));
        }
        cursor += 1;
    }
    Ok(out)
}

fn check_worktree_entry_raw(
    clean_repo: &Path,
    rel: &str,
    expected: &TreeBlobEntry,
    expected_bytes: &[u8],
    check_filemode: bool,
) -> GitResult<bool> {
    let full = resolve_safe_path(clean_repo, rel)?;
    let meta = match fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(GitError::io(&full, e)),
    };
    if expected.mode == "120000" {
        if !meta.file_type().is_symlink() {
            return Ok(false);
        }
        let target = fs::read_link(&full).map_err(|e| GitError::io(&full, e))?;
        return Ok(target.as_os_str().as_encoded_bytes() == expected_bytes);
    }
    if expected.mode != "100644" && expected.mode != "100755" {
        return Ok(false);
    }
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    if check_filemode {
        use std::os::unix::fs::PermissionsExt;
        let is_exec = (meta.permissions().mode() & 0o111) != 0;
        let expect_exec = expected.mode == "100755";
        if is_exec != expect_exec {
            return Ok(false);
        }
    }
    #[cfg(not(unix))]
    let _ = check_filemode;

    if meta.len() != expected_bytes.len() as u64 {
        return Ok(false);
    }
    let (mut file, actual_len) = open_regular_file_no_symlink(&full, rel)?;
    if actual_len != expected_bytes.len() as u64 {
        return Ok(false);
    }
    let mut actual = Vec::with_capacity(expected_bytes.len());
    file.read_to_end(&mut actual)
        .map_err(|e| GitError::io(&full, e))?;
    Ok(actual == expected_bytes)
}

/// Verify that the repository has no in-progress Git operation, that `.git/index` matches
/// `HEAD` exactly (never running `.gitattributes` clean/smudge filters), and that every
/// tracked working-tree file matches its raw `HEAD` blob bytes and mode.
fn ensure_clean_git_state(repo_dir: &Path, git_dir: &Path) -> GitResult<()> {
    ensure_no_in_progress_git_op(git_dir)?;
    let head_sha = git_rev_parse_opt(repo_dir, "HEAD")?;
    let expected_tree = read_commit_tree_entries(repo_dir, head_sha.as_deref())?;
    let index_entries = read_index_entries(repo_dir)?;
    if index_entries != expected_tree {
        if head_sha.is_none() {
            return Err(GitError::InvalidMapping(
                "refusing to export into an unborn Git repository with pre-existing staged files \
                 in the index"
                    .into(),
            ));
        }
        return Err(GitError::InvalidMapping(
            "refusing to export into a Git repository with pre-existing staged changes in the \
             index"
                .into(),
        ));
    }
    if expected_tree.is_empty() {
        return Ok(());
    }
    let oids: Vec<&str> = expected_tree
        .values()
        .filter(|e| matches!(e.mode.as_str(), "100644" | "100755" | "120000"))
        .map(|e| e.oid.as_str())
        .collect();
    let blobs = git_cat_file_batch(repo_dir, &oids)?;
    let check_filemode = git_core_filemode(repo_dir);
    for (rel, entry) in &expected_tree {
        let Some(expected_bytes) = blobs.get(&entry.oid) else {
            return Err(GitError::InvalidMapping(format!(
                "refusing to export into a Git repository with unsupported tracked entry {rel:?} \
                 (mode {})",
                entry.mode
            )));
        };
        if !check_worktree_entry_raw(repo_dir, rel, entry, expected_bytes, check_filemode)? {
            return Err(GitError::InvalidMapping(
                "refusing to export into a Git repository with uncommitted modifications to \
                 tracked files"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn list_tracked_paths_in_commit(
    clean_repo: &Path,
    commit_sha: Option<&str>,
) -> GitResult<HashSet<String>> {
    let entries = read_commit_tree_entries(clean_repo, commit_sha)?;
    Ok(entries.into_keys().collect())
}

fn preflight_worktree_collisions(
    clean_repo: &Path,
    old_tracked: &HashSet<String>,
    new_paths: &[&str],
) -> GitResult<()> {
    for &rel in new_paths {
        let full = resolve_safe_path(clean_repo, rel)?;
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&full, e)),
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: rel.to_string(),
                        reason: "destination path in repository must not be a symlink".into(),
                    });
                }
                if !meta.file_type().is_file() {
                    return Err(GitError::UnsafePath {
                        path: rel.to_string(),
                        reason: "destination path in repository is not a regular file".into(),
                    });
                }
                if !old_tracked.contains(rel) {
                    return Err(GitError::UnsafePath {
                        path: rel.to_string(),
                        reason: format!(
                            "refusing to overwrite unmanaged file {rel:?} in repository working tree"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Synchronize `HEAD`, `.git/index`, and `clean_repo`'s working tree to `commit_sha` on
/// `git_branch` while holding `.git/index.lock`.
///
/// Verifies `.git/index` and every affected working-tree path against raw Git blobs (never
/// running `.gitattributes` clean or smudge filters) before any write or removal, writes
/// exact raw blob bytes, updates `HEAD`, and atomically swaps `.git/index` via `index.lock`.
/// In [`SyncMode::Recovery`], individual old-to-new path transitions and post-sync states
/// are replayable while any unexpected edit, unmanaged file, or staged index entry is
/// refused.
#[allow(clippy::too_many_arguments)]
fn sync_worktree_to_commit_no_filters(
    clean_repo: &Path,
    git_dir: &Path,
    git_branch: &str,
    previous_commit_sha: Option<&str>,
    prev_head_ref: Option<&str>,
    commit_sha: &str,
    mode: SyncMode,
) -> GitResult<()> {
    ensure_no_in_progress_git_op(git_dir)?;
    let index_lock = GitIndexLock::acquire(git_dir)?;

    let old_tree = read_commit_tree_entries(clean_repo, previous_commit_sha)?;
    let target_tree = read_commit_tree_entries(clean_repo, Some(commit_sha))?;
    for (rel, entry) in &target_tree {
        validate_relative_path(rel)?;
        if entry.mode != "100644" && entry.mode != "100755" {
            return Err(GitError::UnsafePath {
                path: rel.clone(),
                reason: format!("unsupported file mode {} in exported Git tree", entry.mode),
            });
        }
    }

    let ref_name = format!("refs/heads/{git_branch}");
    let head_sym = git_symbolic_ref_opt(clean_repo);
    let current_index = read_index_entries(clean_repo)?;
    let index_matches_old = current_index == old_tree;
    let index_matches_new = current_index == target_tree;

    match mode {
        SyncMode::Normal => {
            if !index_matches_old {
                return Err(GitError::InvalidMapping(
                    "refusing worktree sync because the Git index changed concurrently".into(),
                ));
            }
        }
        SyncMode::Recovery => {
            let head_allowed = head_sym.as_deref() == Some(ref_name.as_str())
                || (prev_head_ref.is_some() && head_sym.as_deref() == prev_head_ref)
                || (prev_head_ref.is_none()
                    && head_sym.is_none()
                    && git_rev_parse_opt(clean_repo, "HEAD")?.as_deref() == previous_commit_sha);
            if !head_allowed {
                return Err(GitError::InvalidMapping(format!(
                    "refusing pending recovery because HEAD ({head_sym:?}) moved away from \
                     expected branch {ref_name:?} / {prev_head_ref:?}"
                )));
            }
            if !index_matches_old && !index_matches_new {
                return Err(GitError::InvalidMapping(
                    "refusing pending recovery because the Git index does not match either the \
                     pre-sync or post-sync tree"
                        .into(),
                ));
            }
        }
    }

    let mut all_oids: Vec<&str> = Vec::with_capacity(old_tree.len() + target_tree.len());
    for entry in old_tree.values().chain(target_tree.values()) {
        if matches!(entry.mode.as_str(), "100644" | "100755" | "120000") {
            all_oids.push(entry.oid.as_str());
        }
    }
    let blobs = git_cat_file_batch(clean_repo, &all_oids)?;
    let check_filemode = git_core_filemode(clean_repo);

    // Post-sync recovery: HEAD and index already point to `commit_sha`; verify every target
    // file matches its raw blob bytes so local post-sync edits are never ignored, then
    // finish without misclassifying new files as unmanaged collisions.
    if mode == SyncMode::Recovery
        && index_matches_new
        && head_sym.as_deref() == Some(ref_name.as_str())
    {
        for (rel, target_entry) in &target_tree {
            let new_bytes = blobs.get(&target_entry.oid).ok_or_else(|| {
                GitError::InvalidMapping(format!("missing blob {} for {rel:?}", target_entry.oid))
            })?;
            if !check_worktree_entry_raw(clean_repo, rel, target_entry, new_bytes, check_filemode)?
            {
                return Err(GitError::InvalidMapping(
                    "refusing pending recovery with uncommitted modifications to tracked files"
                        .into(),
                ));
            }
        }
        return Ok(());
    }

    // Read-only eligibility preflight for EVERY target path and EVERY stale old path under
    // `.git/index.lock` BEFORE mutating any working-tree file.
    let mut to_write: Vec<(&str, &[u8])> = Vec::new();
    for (rel, target_entry) in &target_tree {
        let new_bytes = blobs.get(&target_entry.oid).ok_or_else(|| {
            GitError::InvalidMapping(format!("missing blob {} for {rel:?}", target_entry.oid))
        })?;
        let full = resolve_safe_path(clean_repo, rel)?;
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if old_tree.contains_key(rel) {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing worktree sync: previously tracked file {rel:?} was locally \
                         deleted"
                    )));
                }
                to_write.push((rel.as_str(), new_bytes.as_slice()));
            }
            Err(e) => return Err(GitError::io(&full, e)),
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: rel.clone(),
                        reason: "destination path in repository must not be a symlink".into(),
                    });
                }
                if !meta.file_type().is_file() {
                    return Err(GitError::UnsafePath {
                        path: rel.clone(),
                        reason: "destination path in repository is not a regular file".into(),
                    });
                }
                let matches_new = check_worktree_entry_raw(
                    clean_repo,
                    rel,
                    target_entry,
                    new_bytes,
                    check_filemode,
                )?;
                let matches_old = match old_tree.get(rel) {
                    Some(old_entry) => {
                        let old_bytes = blobs.get(&old_entry.oid).ok_or_else(|| {
                            GitError::InvalidMapping(format!(
                                "missing old blob {} for {rel:?}",
                                old_entry.oid
                            ))
                        })?;
                        check_worktree_entry_raw(
                            clean_repo,
                            rel,
                            old_entry,
                            old_bytes,
                            check_filemode,
                        )?
                    }
                    None => false,
                };
                match mode {
                    SyncMode::Normal => {
                        if !old_tree.contains_key(rel) {
                            return Err(GitError::UnsafePath {
                                path: rel.clone(),
                                reason: format!(
                                    "refusing to overwrite unmanaged file {rel:?} in repository \
                                     working tree"
                                ),
                            });
                        }
                        if !matches_old {
                            return Err(GitError::InvalidMapping(format!(
                                "refusing to overwrite locally modified tracked file {rel:?}"
                            )));
                        }
                        if !matches_new {
                            to_write.push((rel.as_str(), new_bytes.as_slice()));
                        }
                    }
                    SyncMode::Recovery => {
                        if matches_new {
                            // Already written by the interrupted sync (or unchanged).
                        } else if matches_old {
                            to_write.push((rel.as_str(), new_bytes.as_slice()));
                        } else if !old_tree.contains_key(rel) {
                            return Err(GitError::UnsafePath {
                                path: rel.clone(),
                                reason: format!(
                                    "refusing to overwrite unmanaged file {rel:?} in repository \
                                     working tree"
                                ),
                            });
                        } else {
                            return Err(GitError::InvalidMapping(format!(
                                "refusing pending recovery: tracked file {rel:?} matches neither \
                                 pre-sync nor post-sync state"
                            )));
                        }
                    }
                }
            }
        }
    }

    let mut to_remove: Vec<(&str, bool)> = Vec::new();
    for (old_rel, old_entry) in &old_tree {
        if target_tree.contains_key(old_rel) {
            continue;
        }
        validate_relative_path(old_rel)?;
        let full = resolve_safe_path(clean_repo, old_rel)?;
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if mode == SyncMode::Normal {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing worktree sync: previously tracked file {old_rel:?} was locally \
                         deleted"
                    )));
                }
                // In recovery mode, the interrupted sync may already have removed this file.
            }
            Err(e) => return Err(GitError::io(&full, e)),
            Ok(meta) => {
                let is_tracked_symlink = old_entry.mode == "120000";
                if meta.file_type().is_symlink() && !is_tracked_symlink {
                    return Err(GitError::UnsafePath {
                        path: old_rel.clone(),
                        reason: "tracked path in repository must not be a symlink".into(),
                    });
                }
                let Some(old_bytes) = blobs.get(&old_entry.oid) else {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to remove unsupported tracked entry {old_rel:?} (mode {})",
                        old_entry.mode
                    )));
                };
                if !check_worktree_entry_raw(
                    clean_repo,
                    old_rel,
                    old_entry,
                    old_bytes,
                    check_filemode,
                )? {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to remove locally modified tracked file {old_rel:?}"
                    )));
                }
                to_remove.push((old_rel.as_str(), is_tracked_symlink));
            }
        }
    }

    // Write every changed tracked blob byte-exact without running Git checkout/smudge filters.
    for (idx, (rel, blob_bytes)) in to_write.into_iter().enumerate() {
        let full = resolve_safe_path(clean_repo, rel)?;
        let parent = effective_parent(&full)?;
        ensure_dir_no_symlink(&parent)?;
        write_atomic_safe(&full, blob_bytes)?;
        #[cfg(test)]
        if idx == 0 {
            test_hooks::after_first_worktree_blob_write()?;
        }
        #[cfg(not(test))]
        let _ = idx;
    }

    // Remove verified previously tracked files absent from the target commit (leaving
    // unmanaged untracked files completely untouched).
    for (idx, (old_rel, is_tracked_symlink)) in to_remove.into_iter().enumerate() {
        let full = resolve_safe_path(clean_repo, old_rel)?;
        match fs::symlink_metadata(&full) {
            Ok(meta) => {
                if meta.file_type().is_symlink() && !is_tracked_symlink {
                    return Err(GitError::UnsafePath {
                        path: old_rel.to_string(),
                        reason: "tracked path in repository must not be a symlink".into(),
                    });
                }
                if meta.file_type().is_file()
                    || (is_tracked_symlink && meta.file_type().is_symlink())
                {
                    fs::remove_file(&full).map_err(|e| GitError::io(&full, e))?;
                    if let Some(p) = full.parent() {
                        let _ = crate::fsutil::sync_dir(p);
                    }
                    #[cfg(test)]
                    if idx == 0 {
                        test_hooks::after_first_worktree_removal()?;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&full, e)),
        }
        #[cfg(not(test))]
        let _ = idx;
    }

    run_git(clean_repo, &["symbolic-ref", "HEAD", &ref_name], &[])?;
    #[cfg(test)]
    test_hooks::after_symbolic_ref_before_index()?;

    // Build the exact target index in an isolated temporary file from the object database
    // (never running filters) and atomically swap it into `.git/index` through our held
    // `.git/index.lock`.
    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let staging_parent = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_STAGING_SUBDIR)?;
    let tmp_index = tempfile::Builder::new()
        .prefix("sync-index-")
        .tempfile_in(&staging_parent)
        .map_err(|e| GitError::io(&staging_parent, e))?;
    let tmp_index_path = tmp_index.into_temp_path();
    let _ = fs::remove_file(&tmp_index_path);
    let tmp_index_str = tmp_index_path.to_string_lossy().into_owned();
    let idx_env = [("GIT_INDEX_FILE", tmp_index_str.as_str())];
    run_git(clean_repo, &["read-tree", commit_sha], &idx_env)?;
    let new_index_bytes =
        fs::read(&tmp_index_path).map_err(|e| GitError::io(&tmp_index_path, e))?;
    drop(tmp_index_path);
    index_lock.commit_index_bytes(git_dir, &new_index_bytes)?;
    Ok(())
}

fn git_rev_parse_opt(repo_dir: &Path, rev: &str) -> GitResult<Option<String>> {
    let out = git_command(repo_dir)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .map_err(|e| GitError::GitCommand {
            args: vec!["rev-parse".into(), rev.into()],
            status: None,
            stderr: e.to_string(),
        })?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

fn run_git(repo_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitResult<String> {
    let bytes = run_git_bytes(repo_dir, args, envs)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn run_git_bytes(repo_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitResult<Vec<u8>> {
    let clean = normalize_dir_for_lstat(repo_dir)?;
    let mut cmd = git_command(&clean);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().map_err(|e| GitError::GitCommand {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        status: None,
        stderr: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(GitError::GitCommand {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

fn run_git_with_stdin(
    repo_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin_bytes: &[u8],
) -> GitResult<String> {
    let bytes = run_git_bytes_with_stdin(repo_dir, args, envs, stdin_bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn run_git_bytes_with_stdin(
    repo_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin_bytes: &[u8],
) -> GitResult<Vec<u8>> {
    let clean = normalize_dir_for_lstat(repo_dir)?;
    let mut cmd = git_command(&clean);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| GitError::GitCommand {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        status: None,
        stderr: e.to_string(),
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_bytes)
            .map_err(|e| GitError::GitCommand {
                args: args.iter().map(|s| (*s).to_string()).collect(),
                status: None,
                stderr: e.to_string(),
            })?;
    }
    let output = child.wait_with_output().map_err(|e| GitError::GitCommand {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        status: None,
        stderr: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(GitError::GitCommand {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

/// Convenience helper for building a [`CommitRef`] from a mapping entry.
impl CommitMappingEntry {
    pub fn commit_ref(&self) -> CommitRef {
        CommitRef {
            branch_id: self.sparkles_branch_id,
            seq: self.sparkles_seq,
        }
    }
}

#[cfg(test)]
mod test_hooks {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub(super) struct FaultState {
        pub fail_before_update_ref: bool,
        pub fail_mapping_write: bool,
        pub fail_worktree_sync: bool,
        pub fail_after_first_blob_write: bool,
        pub fail_after_first_removal: bool,
        pub fail_after_symbolic_ref: bool,
        pub fail_before_remove_pending: bool,
        pub inject_concurrent_stage: Option<(String, Vec<u8>)>,
        pub inject_stage_before_sync: Option<(String, Vec<u8>)>,
        pub inject_tracked_edit_after_clean: Option<(String, Vec<u8>)>,
        pub inject_tracked_edit_before_sync: Option<(String, Vec<u8>)>,
    }

    thread_local! {
        static FAULTS: RefCell<FaultState> = RefCell::new(FaultState::default());
    }

    pub(super) struct FaultGuard;

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            FAULTS.with(|f| *f.borrow_mut() = FaultState::default());
        }
    }

    pub(super) fn install(state: FaultState) -> FaultGuard {
        FAULTS.with(|f| *f.borrow_mut() = state);
        FaultGuard
    }

    pub(super) fn before_update_ref() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_before_update_ref {
                s.fail_before_update_ref = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure before git update-ref"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn during_mapping_write() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_mapping_write {
                s.fail_mapping_write = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure during mapping checkpoint write"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn during_worktree_sync(repo_dir: &Path) -> GitResult<()> {
        let (stage_inj, edit_inj, fail) = FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            let fail = std::mem::take(&mut s.fail_worktree_sync);
            (
                s.inject_stage_before_sync.take(),
                s.inject_tracked_edit_before_sync.take(),
                fail,
            )
        });
        if let Some((rel, bytes)) = stage_inj {
            let p = repo_dir.join(&rel);
            fs::write(&p, bytes).unwrap();
            let status = Command::new("git")
                .args(["-C", repo_dir.to_str().unwrap(), "add", &rel])
                .status()
                .unwrap();
            assert!(status.success());
        }
        if let Some((rel, bytes)) = edit_inj {
            fs::write(repo_dir.join(&rel), bytes).unwrap();
        }
        if fail {
            return Err(GitError::Io {
                path: None,
                source: io::Error::other("injected failure during worktree sync"),
            });
        }
        Ok(())
    }

    pub(super) fn after_first_worktree_blob_write() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_after_first_blob_write {
                s.fail_after_first_blob_write = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure after first worktree blob write"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn after_first_worktree_removal() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_after_first_removal {
                s.fail_after_first_removal = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure after first worktree removal"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn after_symbolic_ref_before_index() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_after_symbolic_ref {
                s.fail_after_symbolic_ref = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure after symbolic-ref before index"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn before_remove_pending() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_before_remove_pending {
                s.fail_before_remove_pending = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure before removing pending journal"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn after_clean_check(repo_dir: &Path) {
        let (stage_inj, edit_inj) = FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            (
                s.inject_concurrent_stage.take(),
                s.inject_tracked_edit_after_clean.take(),
            )
        });
        if let Some((rel, bytes)) = stage_inj {
            let p = repo_dir.join(&rel);
            fs::write(&p, bytes).unwrap();
            let status = Command::new("git")
                .args(["-C", repo_dir.to_str().unwrap(), "add", &rel])
                .status()
                .unwrap();
            assert!(status.success());
        }
        if let Some((rel, bytes)) = edit_inj {
            fs::write(repo_dir.join(&rel), bytes).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::verify_git_tree;
    use oxrdf::{GraphName, Literal, NamedNode, Quad};

    #[test]
    fn author_resolution_handles_formats_and_sanitizes() {
        assert_eq!(
            resolve_author(Some("Alice Example <alice@example.org>"), "Spk", "s@l"),
            ("Alice Example".into(), "alice@example.org".into())
        );
        assert_eq!(
            resolve_author(Some("bob@example.org"), "Spk", "s@l"),
            ("bob@example.org".into(), "bob@example.org".into())
        );
        assert_eq!(
            resolve_author(Some("carol"), "Spk", "s@l"),
            ("carol".into(), "carol@sparkles.local".into())
        );
        assert_eq!(
            resolve_author(None, "Sparkles", "sparkles@localhost"),
            ("Sparkles".into(), "sparkles@localhost".into())
        );
        assert_eq!(
            resolve_author(Some("<<<>>>"), "<<<>>>", "..."),
            ("Sparkles".into(), "sparkles@localhost".into())
        );
        assert_eq!(
            resolve_author(Some("Alice <bad\n>"), "Spk", "s@l"),
            ("Alice".into(), "bad".into())
        );
    }

    #[test]
    fn branch_name_validation_matches_check_ref_format() {
        for bad in [
            "",
            "@",
            "HEAD",
            "-flag",
            "/lead",
            "trail/",
            "trailing.",
            ".hidden",
            "a/.b",
            "a./b",
            "a.lock",
            "a.lock/b",
            "a..b",
            "a//b",
            "a@{b}",
            "a b",
            "a~1",
            "a^2",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
        ] {
            assert!(
                validate_git_branch_name(bad).is_err(),
                "expected invalid branch name for {bad:?}"
            );
        }
        for good in ["main", "head", "feature/vocab-v1", "release/1.2.3", "a_b.c"] {
            assert!(
                validate_git_branch_name(good).is_ok(),
                "expected valid branch name for {good:?}"
            );
        }
    }

    #[test]
    fn extra_trailers_reject_reserved_sparkles_prefix() {
        assert!(validate_extra_trailers(&[("Reviewed-By".into(), "Bot".into())]).is_ok());
        assert!(validate_extra_trailers(&[("Sparkles-Commit".into(), "99".into())]).is_err());
        assert!(validate_extra_trailers(&[("sparkles-branch".into(), "x".into())]).is_err());
        assert!(validate_extra_trailers(&[("-Bad".into(), "x".into())]).is_err());
    }

    #[test]
    fn mapping_refuses_unknown_format_version_and_duplicate_keys() {
        for v in [0u32, 2, 42] {
            let json = format!(r#"{{"format_version": {v}}}"#);
            let err = parse_and_validate_mapping(json.as_bytes(), "mapping.json").unwrap_err();
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
        let dup = r#"{"format_version": 2, "format_version": 1}"#;
        let err = parse_and_validate_mapping(dup.as_bytes(), "mapping.json").unwrap_err();
        assert!(
            matches!(err, GitError::InvalidMapping(ref m) if m.contains("duplicate JSON key")),
            "expected duplicate JSON key error, got {err:?}"
        );
    }

    #[test]
    fn fault_injection_recovers_cleanly_on_both_sides_of_ref_publication() {
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        let insert_n = |n: i64| {
            ds.transaction(|tx| {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(format!("http://example.org/s{n}")),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(n),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                Ok(())
            })
            .unwrap();
        };
        insert_n(1);

        // 1. Failure BEFORE update-ref on first commit -> retry succeeds with 1 commit.
        let repo = tempfile::tempdir().unwrap();
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_before_update_ref: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(err.to_string().contains("before git update-ref"), "{err}");
        }
        let r1 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r1.commits_exported, 1);
        assert_eq!(r1.mapping.entries.len(), 1);
        verify_git_tree(repo.path()).unwrap();

        // 2. Failure AFTER update-ref (during mapping write) on commit 2 -> retry recovers
        //    the already-published commit without duplicating it or requiring manual reset.
        insert_n(2);
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_mapping_write: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(err.to_string().contains("mapping checkpoint"), "{err}");
        }
        let r2 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r2.commits_exported, 0);
        assert_eq!(r2.commits_skipped, 2);
        assert_eq!(r2.mapping.entries.len(), 2);
        let shas = run_git(repo.path(), &["rev-list", "refs/heads/main"], &[]).unwrap();
        assert_eq!(shas.lines().count(), 2, "must not duplicate commit 2");
        verify_git_tree(repo.path()).unwrap();

        // 3. Failure AFTER mapping write (during worktree sync) on commit 3 -> retry finishes
        //    worktree sync cleanly with exact 3 commits.
        insert_n(3);
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_worktree_sync: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(err.to_string().contains("worktree sync"), "{err}");
        }
        let r3 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r3.commits_exported, 0);
        assert_eq!(r3.commits_skipped, 3);
        assert_eq!(r3.mapping.entries.len(), 3);
        let shas = run_git(repo.path(), &["rev-list", "refs/heads/main"], &[]).unwrap();
        assert_eq!(shas.lines().count(), 3);
        verify_git_tree(repo.path()).unwrap();

        // 4. Failure AFTER update-ref on Squash mode -> retry recovers cleanly with 1 commit.
        let repo_sq = tempfile::tempdir().unwrap();
        let sq_opts = GitExportOptions {
            mode: GitExportMode::Squash,
            ..GitExportOptions::default()
        };
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_mapping_write: true,
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo_sq.path(), &sq_opts).is_err());
        }
        let rsq = export_branch_to_git(&ds, repo_sq.path(), &sq_opts).unwrap();
        assert_eq!(rsq.commits_exported, 0);
        assert_eq!(rsq.mapping.entries.len(), 1);
        let sq_shas = run_git(repo_sq.path(), &["rev-list", "refs/heads/main"], &[]).unwrap();
        assert_eq!(sq_shas.lines().count(), 1);
        verify_git_tree(repo_sq.path()).unwrap();
    }

    #[test]
    fn fault_injection_recovers_cleanly_across_mid_sync_post_sync_and_branch_switches() {
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        let insert_g = |g: Option<&str>, n: i64| {
            ds.transaction(|tx| {
                let gn = match g {
                    Some(iri) => GraphName::NamedNode(NamedNode::new_unchecked(iri)),
                    None => GraphName::DefaultGraph,
                };
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(format!("http://example.org/s{n}")),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(n),
                        gn,
                    )
                    .as_ref(),
                )?;
                Ok(())
            })
            .unwrap();
        };
        insert_g(None, 1);
        insert_g(Some("http://example.org/temp-graph"), 10);

        // 1. First (unborn) export fails mid-sync AFTER writing first blob -> retry succeeds.
        let repo = tempfile::tempdir().unwrap();
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_after_first_blob_write: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(
                err.to_string().contains("first worktree blob write"),
                "{err}"
            );
        }
        let r1 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r1.commits_skipped, 1);
        assert_eq!(r1.commits_exported, 1);
        assert_eq!(r1.mapping.entries.len(), 2);
        verify_git_tree(repo.path()).unwrap();

        // 2. Post-sync crash (right before removing pending journal) on a commit that adds a
        //    brand-new graph file -> retry succeeds without misclassifying the new file.
        insert_g(Some("http://example.org/new-graph"), 20);
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_before_remove_pending: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(
                err.to_string().contains("removing pending journal"),
                "{err}"
            );
        }
        let r2 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r2.commits_exported, 0);
        assert_eq!(r2.mapping.entries.len(), 3);
        verify_git_tree(repo.path()).unwrap();

        // 3. Commit that drops `temp-graph` fails mid-sync right AFTER removing the stale graph
        //    file -> retry succeeds cleanly.
        ds.update("DROP GRAPH <http://example.org/temp-graph>")
            .unwrap();
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_after_first_removal: true,
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(err.to_string().contains("first worktree removal"), "{err}");
        }
        let r3 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r3.commits_exported, 0);
        assert_eq!(r3.mapping.entries.len(), 4);
        verify_git_tree(repo.path()).unwrap();

        // 4. Branch switch from `main` to `feat` fails AFTER `symbolic-ref HEAD` and BEFORE
        //    swapping `.git/index` -> retry recovers prior checkout identity and finishes.
        ds.create_branch("feat", &crate::branch::BranchOptions::default())
            .unwrap();
        let feat = ds.branch("feat").unwrap();
        feat.transaction(|tx| {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked("http://example.org/feat-s"),
                    NamedNode::new_unchecked("http://example.org/p"),
                    Literal::from(99i64),
                    GraphName::NamedNode(NamedNode::new_unchecked("http://example.org/feat-g")),
                )
                .as_ref(),
            )?;
            Ok(())
        })
        .unwrap();
        let feat_opts = GitExportOptions {
            branch: Some("feat".into()),
            ..GitExportOptions::default()
        };
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_after_symbolic_ref: true,
                ..Default::default()
            });
            let err = export_branch_to_git(&ds, repo.path(), &feat_opts).unwrap_err();
            assert!(
                err.to_string().contains("symbolic-ref before index"),
                "{err}"
            );
        }
        let r_feat = export_branch_to_git(&ds, repo.path(), &feat_opts).unwrap();
        assert!(!r_feat.mapping.entries.is_empty());
        verify_git_tree(repo.path()).unwrap();
        assert_eq!(
            git_symbolic_ref_opt(repo.path()).as_deref(),
            Some("refs/heads/feat")
        );

        // 5. Journaled no-op branch switch back from `feat` to `main` (0 new commits) fails
        //    mid-sync after first blob write, then fails after symbolic-ref -> retry recovers
        //    cleanly and lands on `main`.
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_after_first_blob_write: true,
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        // Exporting `feat` while `main` has an unreconciled pending journal must refuse.
        assert!(export_branch_to_git(&ds, repo.path(), &feat_opts).is_err());
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_after_symbolic_ref: true,
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        let r_back = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r_back.commits_exported, 0);
        assert_eq!(
            git_symbolic_ref_opt(repo.path()).as_deref(),
            Some("refs/heads/main")
        );
        verify_git_tree(repo.path()).unwrap();
    }

    #[test]
    fn concurrent_staging_and_tracked_edits_are_preserved_before_and_after_ref_update() {
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        let insert_n = |n: i64| {
            ds.transaction(|tx| {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(format!("http://example.org/s{n}")),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(n),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                Ok(())
            })
            .unwrap();
        };
        insert_n(1);

        // 1. Concurrent `git add SNEAKY.txt` after initial clean check on unborn branch:
        //    export aborts before moving ref AND `SNEAKY.txt` remains staged in `.git/index`.
        let repo = tempfile::tempdir().unwrap();
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_concurrent_stage: Some(("SNEAKY.txt".into(), b"do not lose me\n".to_vec())),
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        let staged_after = run_git(repo.path(), &["ls-files", "--cached"], &[]).unwrap();
        assert!(
            staged_after.contains("SNEAKY.txt"),
            "concurrently staged file must remain staged in .git/index: {staged_after:?}"
        );
        assert!(
            git_rev_parse_opt(repo.path(), "refs/heads/main")
                .unwrap()
                .is_none(),
            "ref must not advance when index changed before update-ref"
        );
        // Unstage SNEAKY.txt and complete initial export cleanly.
        run_git(repo.path(), &["rm", "--cached", "SNEAKY.txt"], &[]).unwrap();
        fs::remove_file(repo.path().join("SNEAKY.txt")).unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();

        // 2. Concurrent `git add SNEAKY2.txt` AFTER update-ref (right before worktree sync):
        //    commit does NOT contain SNEAKY2.txt, sync refuses to wipe `.git/index`, and
        //    `SNEAKY2.txt` remains staged until the caller clears it.
        insert_n(2);
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_stage_before_sync: Some((
                    "SNEAKY2.txt".into(),
                    b"staged after update-ref\n".to_vec(),
                )),
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        let commit2_files = run_git(
            repo.path(),
            &["ls-tree", "-r", "--name-only", "refs/heads/main"],
            &[],
        )
        .unwrap();
        assert!(
            !commit2_files.contains("SNEAKY2.txt"),
            "concurrently staged file leaked into published commit: {commit2_files}"
        );
        let staged2 = run_git(repo.path(), &["ls-files", "--cached"], &[]).unwrap();
        assert!(
            staged2.contains("SNEAKY2.txt"),
            "SNEAKY2.txt must remain staged in .git/index after sync refusal: {staged2:?}"
        );
        run_git(repo.path(), &["rm", "--cached", "SNEAKY2.txt"], &[]).unwrap();
        fs::remove_file(repo.path().join("SNEAKY2.txt")).unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        verify_git_tree(repo.path()).unwrap();

        // 3. Concurrent unstaged edit to previously tracked `graphs/default.ttl` AFTER
        //    update-ref (right before worktree sync): sync refuses and preserves local edit!
        insert_n(3);
        let original_ttl = fs::read(repo.path().join("graphs/default.ttl")).unwrap();
        let local_edit = b"# user local edit in progress\n";
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_tracked_edit_before_sync: Some((
                    "graphs/default.ttl".into(),
                    local_edit.to_vec(),
                )),
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        assert_eq!(
            fs::read(repo.path().join("graphs/default.ttl")).unwrap(),
            local_edit,
            "sync must NOT overwrite concurrent local edit to a tracked file"
        );
        // Restoring original tracked bytes lets pending recovery complete cleanly.
        fs::write(repo.path().join("graphs/default.ttl"), original_ttl).unwrap();
        let r3 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(r3.mapping.entries.len(), 3);
        verify_git_tree(repo.path()).unwrap();
    }

    #[test]
    fn near_limit_mapping_recovers_cleanly_through_compact_pending_journal() {
        // Construct a valid CommitMapping with enough entries that pretty-printing it inside
        // PendingPublication (adding 2-space indentation per nested line) would exceed
        // `standalone_len + 64 KiB`, while compact serialization stays well below the ceiling.
        let ds_id = Uuid::new_v4();
        let br_id = Uuid::new_v4();
        let entries: Vec<CommitMappingEntry> = (1..=4500u64)
            .map(|seq| CommitMappingEntry {
                sparkles_branch_id: br_id,
                sparkles_branch: Some("main".to_string()),
                sparkles_seq: seq,
                git_sha: format!("{seq:040x}"),
                mode: GitExportMode::Commits,
                timestamp_ms: 1_700_000_000_000 + seq as i64,
                squashed_from: None,
            })
            .collect();
        let mapping = CommitMapping {
            format_version: MAPPING_FORMAT_VERSION,
            profile: MAPPING_PROFILE_NAME.to_string(),
            dataset_id: ds_id,
            branch: "main".to_string(),
            branch_id: br_id,
            git_branch: "main".to_string(),
            syntax: CanonicalSyntax::Turtle,
            entries,
        };
        let standalone = mapping.serialize_bounded(u64::MAX).unwrap();
        let exact_limit = standalone.len() as u64;
        assert!(mapping.serialize_bounded(exact_limit).is_ok());

        let pending = PendingPublication {
            format_version: MAPPING_FORMAT_VERSION,
            mapping_path: PathBuf::from("/tmp/mapping.json"),
            ref_name: "refs/heads/main".to_string(),
            old_ref_sha: Some(format!("{:040x}", 4499u64)),
            prev_checkout_sha: Some(format!("{:040x}", 4499u64)),
            prev_head_ref: Some("refs/heads/main".to_string()),
            new_commit_sha: format!("{:040x}", 4500u64),
            mapping: Some(mapping),
        };
        let pretty_nested = serde_json::to_vec_pretty(&pending).unwrap();
        assert!(
            pretty_nested.len() as u64 > max_pending_journal_bytes(exact_limit),
            "test setup must exceed old pretty-printed journal ceiling"
        );
        let compact = serialize_pending_bounded(&pending, exact_limit).unwrap();
        assert!(
            compact.len() as u64 <= max_pending_journal_bytes(exact_limit),
            "compact pending journal ({}) must fit within reader ceiling ({})",
            compact.len(),
            max_pending_journal_bytes(exact_limit)
        );
        let parsed: PendingPublication = serde_json::from_slice(&compact).unwrap();
        assert_eq!(parsed.mapping.unwrap().entries.len(), 4500);
    }
}
