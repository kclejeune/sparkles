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
    resolve_safe_path, resolve_safe_path_allow_leaf_symlink, sanitize_slug, sha256_hex,
    validate_relative_path, verify_existing_dir_no_symlink, verify_no_symlink_components,
    write_atomic_safe, write_snapshot_git_tree_tracked,
};
use crate::git::{GitError, GitResult};
use crate::history::{At, HistoryOptions};
use crate::store::{BranchCommit, Snapshot, Store};
use serde::{Deserialize, Serialize};
use sparkles_core::jobs::Sha256Stream;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use uuid::Uuid;

/// Supported format version of the bidirectional commit mapping record.
pub const MAPPING_FORMAT_VERSION: u32 = 1;
/// Supported format version of crash-recoverable pending publication journals
/// (versioned independently of [`MAPPING_FORMAT_VERSION`]).
const PENDING_PUBLICATION_FORMAT_VERSION: u32 = 2;
/// Prefix written inside `.git/index.lock` to prove Sparkles ownership during crash recovery.
const INDEX_LOCK_PREFIX: &str = "sparkles-git-index-lock-v1:";
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
    /// Unique ownership token recorded inside `.git/index.lock` while this publication
    /// holds the Git index lock, allowing crash recovery to reclaim its own stale lock
    /// while refusing active or foreign locks.
    index_lock_token: String,
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

    let custom_mapping = options.mapping_path.is_some();
    let mapping_path = match &options.mapping_path {
        Some(p) => resolve_mapping_path_readonly(p)?,
        None => {
            let sparkles_dir = ensure_child_dir_no_symlink(&git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
            let mappings_dir = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_MAPPINGS_SUBDIR)?;
            let slug = sanitize_slug(&git_branch);
            let hash = sha256_hex(git_branch.as_bytes());
            mappings_dir.join(format!("{slug}-{}.json", &hash[..16]))
        }
    };
    validate_mapping_destination_no_overlap(
        &mapping_path,
        &canon_repo,
        &git_dir,
        &pending_path,
        custom_mapping,
    )?;

    // Refuse if `mapping_path` aliases any file currently tracked on `HEAD` BEFORE creating
    // directories, running recovery, or publishing commits.
    let initial_head = git_rev_parse_opt(&clean_repo, "HEAD")?;
    let initial_tracked = list_tracked_paths_in_commit(&clean_repo, initial_head.as_deref())?;
    ensure_mapping_not_tracked(&mapping_path, &canon_repo, &initial_tracked)?;

    // Refuse if another branch has an unreconciled interrupted publication pending.
    ensure_no_other_pending_publications(&git_dir, &pending_path)?;

    // Reconcile any interrupted previous publication on this branch before checking clean
    // repository state, so a failure on either side of `git update-ref` or mid-checkout
    // recovers cleanly without manual resets or duplicate commits.
    reconcile_pending_publication(
        &clean_repo,
        &canon_repo,
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
            // Verify `mapping_path` aliases neither the currently checked-out branch's
            // tracked files (which the switch would unlink) nor the target commit's files.
            let old_tracked = list_tracked_paths_in_commit(&clean_repo, prev_head.as_deref())?;
            let target_tracked = list_tracked_paths_in_commit(&clean_repo, Some(&head_git_sha))?;
            ensure_mapping_not_tracked(&mapping_path, &canon_repo, &old_tracked)?;
            ensure_mapping_not_tracked(&mapping_path, &canon_repo, &target_tracked)?;
            ensure_clean_git_state(&clean_repo, &git_dir)?;
            let lock_token = Uuid::new_v4().to_string();
            let pending = PendingPublication {
                format_version: PENDING_PUBLICATION_FORMAT_VERSION,
                index_lock_token: lock_token.clone(),
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
                &lock_token,
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

    // 6. Ensure the validated mapping parent directory exists (now that all overlap,
    //    tracked-path, and clean checks have succeeded) and persist the crash-recoverable
    //    pending publication record BEFORE moving the ref.
    let mapping_parent = effective_parent(mapping_path)?;
    ensure_dir_no_symlink(&mapping_parent)?;
    let lock_token = Uuid::new_v4().to_string();
    let pending = PendingPublication {
        format_version: PENDING_PUBLICATION_FORMAT_VERSION,
        index_lock_token: lock_token.clone(),
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
        &lock_token,
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
    canon_repo: &Path,
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
    if found != PENDING_PUBLICATION_FORMAT_VERSION {
        return Err(GitError::UnsupportedFormatVersion {
            file: label,
            found,
            supported: PENDING_PUBLICATION_FORMAT_VERSION,
        });
    }
    let pending: PendingPublication = serde_json::from_value(raw_val).map_err(|e| {
        GitError::InvalidMapping(format!("invalid pending publication record: {e}"))
    })?;
    if pending.index_lock_token.is_empty()
        || pending.index_lock_token.len() > 128
        || pending
            .index_lock_token
            .bytes()
            .any(|b| b == b'\n' || b == b'\r' || b == 0)
    {
        return Err(GitError::InvalidMapping(
            "pending publication record has invalid index_lock_token".into(),
        ));
    }
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

    // Verify `mapping_path` never aliases any tracked file in the prior or target commits.
    let old_tracked =
        list_tracked_paths_in_commit(clean_repo, pending.prev_checkout_sha.as_deref())?;
    let target_tracked = list_tracked_paths_in_commit(clean_repo, Some(&pending.new_commit_sha))?;
    ensure_mapping_not_tracked(mapping_path, canon_repo, &old_tracked)?;
    ensure_mapping_not_tracked(mapping_path, canon_repo, &target_tracked)?;

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
    // checkout identity and lock ownership token, accepting files matching either the prior
    // or target blob while refusing any local edits, unmanaged collisions, or foreign locks.
    sync_worktree_to_commit_no_filters(
        clean_repo,
        git_dir,
        git_branch,
        pending.prev_checkout_sha.as_deref(),
        pending.prev_head_ref.as_deref(),
        &pending.new_commit_sha,
        &pending.index_lock_token,
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

/// Resolve a custom mapping path read-only (WITHOUT creating any missing parent
/// directories), rejecting symlinks on existing ancestors or the destination file, and
/// returning one canonical absolute destination path for overlap checks, journals, and
/// persistence.
fn resolve_mapping_path_readonly(path: &Path) -> GitResult<PathBuf> {
    let parent = effective_parent(path)?;
    let label = path.display().to_string();
    verify_no_symlink_components(&parent, &label)?;
    let file_name = path.file_name().ok_or_else(|| GitError::UnsafePath {
        path: label.clone(),
        reason: "mapping path must name a file".into(),
    })?;

    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = parent;
    let existing_base = loop {
        match fs::symlink_metadata(&probe) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: label,
                        reason: format!(
                            "mapping ancestor {} must not be a symlink",
                            probe.display()
                        ),
                    });
                }
                if !meta.file_type().is_dir() {
                    return Err(GitError::UnsafePath {
                        path: label,
                        reason: format!("mapping ancestor {} is not a directory", probe.display()),
                    });
                }
                break probe;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name() else {
                    return Err(GitError::io(&probe, e));
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
            Err(e) => return Err(GitError::io(&probe, e)),
        }
    };

    let mut canon_parent =
        fs::canonicalize(&existing_base).map_err(|e| GitError::io(&existing_base, e))?;
    let parent_existed = missing.is_empty();
    for seg in missing.into_iter().rev() {
        canon_parent.push(seg);
    }
    let resolved = canon_parent.join(file_name);
    if parent_existed {
        check_dest_file_not_symlink(&resolved)?;
    }
    Ok(resolved)
}

/// Refuse any mapping destination that aliases internal Git/pending metadata or published
/// canonical tree directories (`.sparkles-git/`, `graphs/`). Custom `--mapping` paths are
/// never permitted anywhere inside `.git/`.
fn validate_mapping_destination_no_overlap(
    mapping_path: &Path,
    canon_repo: &Path,
    git_dir: &Path,
    pending_path: &Path,
    is_custom_mapping: bool,
) -> GitResult<()> {
    let sparkles_dir = git_dir.join(DEFAULT_SPARKLES_GIT_SUBDIR);
    let pending_dir = sparkles_dir.join(DEFAULT_PENDING_SUBDIR);
    let staging_dir = sparkles_dir.join(DEFAULT_STAGING_SUBDIR);
    let lock_path = sparkles_dir.join("export.lock");
    let forbidden_reason = if is_custom_mapping
        && (mapping_path == git_dir || mapping_path.starts_with(git_dir))
    {
        Some("custom mapping path must not lie inside internal Git metadata directory (.git/)")
    } else if mapping_path == pending_path || mapping_path.starts_with(&pending_dir) {
        Some("mapping path must not alias Sparkles pending publication directory")
    } else if mapping_path.starts_with(&staging_dir) {
        Some("mapping path must not alias Sparkles staging directory")
    } else if mapping_path == lock_path {
        Some("mapping path must not alias Sparkles export lock")
    } else if mapping_path == git_dir.join("index")
        || mapping_path == git_dir.join("index.lock")
        || mapping_path == git_dir.join("HEAD")
        || mapping_path == git_dir.join("HEAD.lock")
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

/// Verify that `mapping_path` does not alias any tracked path in `tracked_paths`.
fn ensure_mapping_not_tracked(
    mapping_path: &Path,
    canon_repo: &Path,
    tracked_paths: &HashSet<String>,
) -> GitResult<()> {
    for rel in tracked_paths {
        if mapping_path == canon_repo.join(rel) {
            return Err(GitError::UnsafePath {
                path: mapping_path.display().to_string(),
                reason: format!("mapping path must not alias repository tracked path {rel:?}"),
            });
        }
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

fn index_lock_payload(token: &str) -> Vec<u8> {
    format!("{INDEX_LOCK_PREFIX}{token}\n").into_bytes()
}

/// Exclusive lock on `<git-dir>/index.lock` following Git's lockfile protocol so external
/// `git add`/`git rm`/`git checkout` cannot race with index inspection, worktree sync, or
/// the atomic swap of `.git/index`.
///
/// The lock file contains a unique per-publication ownership token (`sparkles-git-index-lock-v1:<token>\n`)
/// and is also held with an OS advisory lock (`File::try_lock`), installed atomically via
/// `hard_link` from `<git-dir>/sparkles/staging/`. If a process crashes abruptly while
/// holding `.git/index.lock`, recovery under [`SyncMode::Recovery`] verifies both exact
/// token ownership and that no live process holds the OS lock before reclaiming it, while
/// refusing any active or foreign `.git/index.lock`.
struct GitIndexLock {
    lock_path: PathBuf,
    _file: Option<File>,
    committed: bool,
}

impl GitIndexLock {
    fn acquire(git_dir: &Path, lock_token: &str, allow_reclaim: bool) -> GitResult<Self> {
        let lock_path = git_dir.join("index.lock");
        let payload = index_lock_payload(lock_token);
        let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
        let staging_dir = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_STAGING_SUBDIR)?;

        // Stage the ownership payload into a temp file, sync it, and lock its inode BEFORE
        // linking it into `.git/index.lock` so `.git/index.lock` is never visible empty.
        let mut tmp = tempfile::Builder::new()
            .prefix("index-lock-")
            .tempfile_in(&staging_dir)
            .map_err(|e| GitError::io(&staging_dir, e))?;
        tmp.as_file_mut()
            .write_all(&payload)
            .map_err(|e| GitError::io(tmp.path(), e))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| GitError::io(tmp.path(), e))?;
        tmp.as_file()
            .try_lock()
            .map_err(|e| GitError::io(tmp.path(), io::Error::other(e.to_string())))?;

        match fs::hard_link(tmp.path(), &lock_path) {
            Ok(()) => {
                let (file, tmp_path) = tmp.into_parts();
                let _ = fs::remove_file(&tmp_path);
                crate::fsutil::sync_dir(git_dir).map_err(|e| GitError::io(git_dir, e))?;
                Ok(Self {
                    lock_path,
                    _file: Some(file),
                    committed: false,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                drop(tmp);
                if allow_reclaim {
                    Self::try_reclaim(&lock_path, &payload)
                } else {
                    Err(Self::busy_error(&lock_path))
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Unsupported
                        | io::ErrorKind::CrossesDevices
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                drop(tmp);
                Self::acquire_create_new(git_dir, &lock_path, &payload, allow_reclaim)
            }
            Err(e) => Err(GitError::io(&lock_path, e)),
        }
    }

    fn acquire_create_new(
        git_dir: &Path,
        lock_path: &Path,
        payload: &[u8],
        allow_reclaim: bool,
    ) -> GitResult<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = match opts.open(lock_path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if allow_reclaim {
                    return Self::try_reclaim(lock_path, payload);
                }
                return Err(Self::busy_error(lock_path));
            }
            Err(e) => return Err(GitError::io(lock_path, e)),
        };
        file.try_lock()
            .map_err(|e| GitError::io(lock_path, io::Error::other(e.to_string())))?;
        file.write_all(payload)
            .map_err(|e| GitError::io(lock_path, e))?;
        file.sync_all().map_err(|e| GitError::io(lock_path, e))?;
        crate::fsutil::sync_dir(git_dir).map_err(|e| GitError::io(git_dir, e))?;
        Ok(Self {
            lock_path: lock_path.to_path_buf(),
            _file: Some(file),
            committed: false,
        })
    }

    fn try_reclaim(lock_path: &Path, expected_payload: &[u8]) -> GitResult<Self> {
        let lmeta = match fs::symlink_metadata(lock_path) {
            Ok(m) => m,
            Err(e) => return Err(GitError::io(lock_path, e)),
        };
        if lmeta.file_type().is_symlink() || !lmeta.file_type().is_file() {
            return Err(Self::busy_error(lock_path));
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = match opts.open(lock_path) {
            Ok(f) => f,
            Err(_) => return Err(Self::busy_error(lock_path)),
        };
        let fmeta = file.metadata().map_err(|e| GitError::io(lock_path, e))?;
        if !fmeta.file_type().is_file() {
            return Err(Self::busy_error(lock_path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (fmeta.dev(), fmeta.ino()) != (lmeta.dev(), lmeta.ino()) {
                return Err(Self::busy_error(lock_path));
            }
        }
        // Refuse if any live process still holds the OS lock on this inode.
        if file.try_lock().is_err() {
            return Err(Self::busy_error(lock_path));
        }
        let mut buf = Vec::with_capacity(expected_payload.len() + 1);
        Read::by_ref(&mut file)
            .take((expected_payload.len() as u64).saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| GitError::io(lock_path, e))?;
        if buf != expected_payload {
            return Err(Self::busy_error(lock_path));
        }
        // Confirm the directory entry still refers to the exact inode we locked and verified.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let lmeta2 = fs::symlink_metadata(lock_path).map_err(|e| GitError::io(lock_path, e))?;
            if lmeta2.file_type().is_symlink()
                || (fmeta.dev(), fmeta.ino()) != (lmeta2.dev(), lmeta2.ino())
            {
                return Err(Self::busy_error(lock_path));
            }
        }
        Ok(Self {
            lock_path: lock_path.to_path_buf(),
            _file: Some(file),
            committed: false,
        })
    }

    fn busy_error(lock_path: &Path) -> GitError {
        GitError::InvalidMapping(format!(
            "refusing to modify Git index while {} exists (another Git operation is in progress)",
            lock_path.display()
        ))
    }

    /// Atomically install the already-built `staged_index_path` into `<git-dir>/index`
    /// while `.git/index.lock` continues to hold our ownership token, then release the lock.
    fn commit_index_from_path(mut self, git_dir: &Path, staged_index_path: &Path) -> GitResult<()> {
        let f = File::open(staged_index_path).map_err(|e| GitError::io(staged_index_path, e))?;
        f.sync_all()
            .map_err(|e| GitError::io(staged_index_path, e))?;
        drop(f);

        let index_path = git_dir.join("index");
        check_dest_file_not_symlink(&index_path)?;
        match fs::rename(staged_index_path, &index_path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let bytes = fs::read(staged_index_path)
                    .map_err(|err| GitError::io(staged_index_path, err))?;
                write_atomic_safe(&index_path, &bytes)?;
                let _ = fs::remove_file(staged_index_path);
            }
            Err(e) => return Err(GitError::io(&index_path, e)),
        }
        crate::fsutil::sync_dir(git_dir).map_err(|e| GitError::io(git_dir, e))?;
        match fs::remove_file(&self.lock_path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&self.lock_path, e)),
        }
        crate::fsutil::sync_dir(git_dir).map_err(|e| GitError::io(git_dir, e))?;
        self.committed = true;
        self._file.take();
        Ok(())
    }
}

impl Drop for GitIndexLock {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.lock_path);
            if let Some(parent) = self.lock_path.parent() {
                let _ = crate::fsutil::sync_dir(parent);
            }
        }
        self._file.take();
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

/// Stream blobs from `git cat-file --batch` in request order without deadlocking on pipe
/// buffers and without materializing the batch stdout in memory.
///
/// Feeds OIDs into `stdin` on a scoped writer thread and drains bounded `stderr` on a
/// second scoped thread while the calling thread streams each `<oid> blob <size>\n<body>\n`
/// entry directly into `on_blob`.
fn stream_git_blobs<F>(clean_repo: &Path, oids: &[&str], mut on_blob: F) -> GitResult<()>
where
    F: FnMut(usize, &str, u64, &mut dyn Read) -> GitResult<()>,
{
    if oids.is_empty() {
        return Ok(());
    }
    let clean = normalize_dir_for_lstat(clean_repo)?;
    let mut cmd = git_command(&clean);
    cmd.args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| GitError::GitCommand {
        args: vec!["cat-file".into(), "--batch".into()],
        status: None,
        stderr: e.to_string(),
    })?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let mut reader_err: Option<GitError> = None;
    let (write_res, stderr_bytes) = std::thread::scope(|scope| {
        let writer_handle = scope.spawn(move || -> io::Result<()> {
            let mut bw = BufWriter::new(stdin);
            for oid in oids {
                if let Err(e) = bw
                    .write_all(oid.as_bytes())
                    .and_then(|()| bw.write_all(b"\n"))
                {
                    if e.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    return Err(e);
                }
            }
            if let Err(e) = bw.flush()
                && e.kind() != io::ErrorKind::BrokenPipe
            {
                return Err(e);
            }
            Ok(())
        });
        let stderr_handle = scope.spawn(move || -> Vec<u8> {
            let mut buf = Vec::new();
            let mut limited = stderr.take(64 * 1024);
            let _ = limited.read_to_end(&mut buf);
            let mut rest = limited.into_inner();
            let _ = io::copy(&mut rest, &mut io::sink());
            buf
        });

        let mut reader = BufReader::with_capacity(64 * 1024, stdout);
        let mut header_buf = Vec::with_capacity(128);
        for (idx, &req_oid) in oids.iter().enumerate() {
            header_buf.clear();
            let read_hdr = read_line_bounded(&mut reader, &mut header_buf, 1024);
            let n = match read_hdr {
                Ok(n) => n,
                Err(e) => {
                    reader_err = Some(e);
                    break;
                }
            };
            if n == 0 {
                reader_err = Some(GitError::InvalidMapping(format!(
                    "truncated git cat-file --batch header for {req_oid}"
                )));
                break;
            }
            let header = match std::str::from_utf8(&header_buf) {
                Ok(s) => s.trim_end_matches(['\r', '\n']),
                Err(_) => {
                    reader_err = Some(GitError::InvalidMapping(
                        "non-UTF-8 git cat-file header".into(),
                    ));
                    break;
                }
            };
            let parts: Vec<&str> = header.split_whitespace().collect();
            if parts.len() != 3 || parts[1] != "blob" {
                reader_err = Some(GitError::InvalidMapping(format!(
                    "unexpected git cat-file --batch entry for {req_oid}: {header:?}"
                )));
                break;
            }
            let size: u64 = match parts[2].parse() {
                Ok(sz) => sz,
                Err(_) => {
                    reader_err = Some(GitError::InvalidMapping(format!(
                        "invalid git cat-file --batch size for {req_oid}: {:?}",
                        parts[2]
                    )));
                    break;
                }
            };
            let mut limited = reader.by_ref().take(size);
            if let Err(e) = on_blob(idx, req_oid, size, &mut limited) {
                reader_err = Some(e);
                break;
            }
            if limited.limit() > 0 {
                match io::copy(&mut limited, &mut io::sink()) {
                    Ok(_) if limited.limit() == 0 => {}
                    Ok(_) => {
                        reader_err = Some(GitError::InvalidMapping(format!(
                            "truncated git cat-file --batch body for {req_oid}"
                        )));
                        break;
                    }
                    Err(e) => {
                        reader_err = Some(GitError::GitCommand {
                            args: vec!["cat-file".into(), "--batch".into()],
                            status: None,
                            stderr: e.to_string(),
                        });
                        break;
                    }
                }
            }
            let mut nl = [0u8; 1];
            match reader.read_exact(&mut nl) {
                Ok(()) if nl[0] == b'\n' => {}
                Ok(()) => {
                    reader_err = Some(GitError::InvalidMapping(
                        "missing trailing newline in git cat-file --batch output".into(),
                    ));
                    break;
                }
                Err(_) => {
                    reader_err = Some(GitError::InvalidMapping(format!(
                        "truncated trailing newline in git cat-file --batch output for {req_oid}"
                    )));
                    break;
                }
            }
        }
        drop(reader);
        if reader_err.is_some() {
            let _ = child.kill();
        }
        let write_res = writer_handle
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("cat-file stdin writer thread panicked")));
        let stderr_bytes = stderr_handle.join().unwrap_or_default();
        (write_res, stderr_bytes)
    });

    let status = child.wait().map_err(|e| GitError::GitCommand {
        args: vec!["cat-file".into(), "--batch".into()],
        status: None,
        stderr: e.to_string(),
    })?;
    if let Some(err) = reader_err {
        return Err(err);
    }
    write_res.map_err(|e| GitError::GitCommand {
        args: vec!["cat-file".into(), "--batch".into()],
        status: status.code(),
        stderr: e.to_string(),
    })?;
    if !status.success() {
        return Err(GitError::GitCommand {
            args: vec!["cat-file".into(), "--batch".into()],
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        });
    }
    Ok(())
}

fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max_len: usize,
) -> GitResult<usize> {
    let mut total = 0usize;
    loop {
        let avail = reader.fill_buf().map_err(|e| GitError::GitCommand {
            args: vec!["cat-file".into(), "--batch".into()],
            status: None,
            stderr: e.to_string(),
        })?;
        if avail.is_empty() {
            return Ok(total);
        }
        let (take, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(pos) => (pos + 1, true),
            None => (avail.len(), false),
        };
        if total + take > max_len {
            return Err(GitError::InvalidMapping(
                "git cat-file --batch header exceeds maximum length".into(),
            ));
        }
        out.extend_from_slice(&avail[..take]);
        reader.consume(take);
        total += take;
        if done {
            return Ok(total);
        }
    }
}

#[derive(Clone, Debug)]
struct ExpectedFileContent {
    mode: String,
    len: u64,
    sha256: String,
}

fn drain_and_hash_reader(reader: &mut dyn Read, hasher: &mut Sha256Stream) -> io::Result<u64> {
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}

/// Stream-compare a working-tree path against a raw Git blob reader in bounded chunks while
/// computing the blob's SHA-256 digest.
///
/// Uses [`resolve_safe_path_allow_leaf_symlink`] for tracked `120000` symlinks and
/// [`resolve_safe_path`] for regular files, and checks Git's owner execute bit (`0o100`).
fn stream_compare_and_hash_worktree_entry(
    clean_repo: &Path,
    rel: &str,
    expected: &TreeBlobEntry,
    blob_size: u64,
    blob_reader: &mut dyn Read,
    check_filemode: bool,
) -> GitResult<(bool, String)> {
    let mut hasher = Sha256Stream::new();
    if expected.mode == "120000" {
        const MAX_SYMLINK_TARGET_BYTES: u64 = 64 * 1024;
        if blob_size > MAX_SYMLINK_TARGET_BYTES {
            let _ = drain_and_hash_reader(blob_reader, &mut hasher);
            return Ok((false, hasher.finalize_hex()));
        }
        let mut target_blob = Vec::with_capacity(blob_size as usize);
        blob_reader
            .read_to_end(&mut target_blob)
            .map_err(|e| GitError::GitCommand {
                args: vec!["cat-file".into(), "--batch".into()],
                status: None,
                stderr: e.to_string(),
            })?;
        hasher.update(&target_blob);
        let digest = hasher.finalize_hex();
        let full = resolve_safe_path_allow_leaf_symlink(clean_repo, rel)?;
        let meta = match fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((false, digest)),
            Err(e) => return Err(GitError::io(&full, e)),
        };
        if !meta.file_type().is_symlink() {
            return Ok((false, digest));
        }
        let target = fs::read_link(&full).map_err(|e| GitError::io(&full, e))?;
        return Ok((
            target.as_os_str().as_encoded_bytes() == target_blob.as_slice(),
            digest,
        ));
    }

    if expected.mode != "100644" && expected.mode != "100755" {
        let _ = drain_and_hash_reader(blob_reader, &mut hasher);
        return Ok((false, hasher.finalize_hex()));
    }

    let full = resolve_safe_path(clean_repo, rel)?;
    let meta = match fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let _ = drain_and_hash_reader(blob_reader, &mut hasher);
            return Ok((false, hasher.finalize_hex()));
        }
        Err(e) => return Err(GitError::io(&full, e)),
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        let _ = drain_and_hash_reader(blob_reader, &mut hasher);
        return Ok((false, hasher.finalize_hex()));
    }
    #[cfg(unix)]
    if check_filemode {
        use std::os::unix::fs::PermissionsExt;
        // Git maps regular files to 100755 vs 100644 strictly using the owner execute bit.
        let is_exec = (meta.permissions().mode() & 0o100) != 0;
        let expect_exec = expected.mode == "100755";
        if is_exec != expect_exec {
            let _ = drain_and_hash_reader(blob_reader, &mut hasher);
            return Ok((false, hasher.finalize_hex()));
        }
    }
    #[cfg(not(unix))]
    let _ = check_filemode;

    if meta.len() != blob_size {
        let _ = drain_and_hash_reader(blob_reader, &mut hasher);
        return Ok((false, hasher.finalize_hex()));
    }
    let (mut file, actual_len) = open_regular_file_no_symlink(&full, rel)?;
    if actual_len != blob_size {
        let _ = drain_and_hash_reader(blob_reader, &mut hasher);
        return Ok((false, hasher.finalize_hex()));
    }

    let mut blob_buf = [0u8; 64 * 1024];
    let mut file_buf = [0u8; 64 * 1024];
    let mut matched = true;
    loop {
        let n = blob_reader
            .read(&mut blob_buf)
            .map_err(|e| GitError::GitCommand {
                args: vec!["cat-file".into(), "--batch".into()],
                status: None,
                stderr: e.to_string(),
            })?;
        if n == 0 {
            break;
        }
        hasher.update(&blob_buf[..n]);
        if matched {
            let mut got = 0usize;
            while got < n {
                let r = file
                    .read(&mut file_buf[got..n])
                    .map_err(|e| GitError::io(&full, e))?;
                if r == 0 {
                    matched = false;
                    break;
                }
                got += r;
            }
            if matched && file_buf[..n] != blob_buf[..n] {
                matched = false;
            }
        }
    }
    if matched {
        let mut extra = [0u8; 1];
        if file.read(&mut extra).map_err(|e| GitError::io(&full, e))? != 0 {
            matched = false;
        }
    }
    Ok((matched, hasher.finalize_hex()))
}

/// Stream-verify an on-disk path against an [`ExpectedFileContent`] without buffering the
/// full file in memory.
fn verify_path_matches_expected(
    full: &Path,
    rel: &str,
    expected: &ExpectedFileContent,
    check_filemode: bool,
) -> GitResult<bool> {
    let meta = match fs::symlink_metadata(full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(GitError::io(full, e)),
    };
    if expected.mode == "120000" {
        if !meta.file_type().is_symlink() {
            return Ok(false);
        }
        let target = fs::read_link(full).map_err(|e| GitError::io(full, e))?;
        let raw = target.as_os_str().as_encoded_bytes();
        return Ok(raw.len() as u64 == expected.len && sha256_hex(raw) == expected.sha256);
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
        let is_exec = (meta.permissions().mode() & 0o100) != 0;
        let expect_exec = expected.mode == "100755";
        if is_exec != expect_exec {
            return Ok(false);
        }
    }
    #[cfg(not(unix))]
    let _ = check_filemode;

    if meta.len() != expected.len {
        return Ok(false);
    }
    let (mut file, actual_len) = open_regular_file_no_symlink(full, rel)?;
    if actual_len != expected.len {
        return Ok(false);
    }
    let mut hasher = Sha256Stream::new();
    let total = drain_and_hash_reader(&mut file, &mut hasher).map_err(|e| GitError::io(full, e))?;
    Ok(total == expected.len && hasher.finalize_hex() == expected.sha256)
}

fn verify_open_file_matches_expected(
    file: &mut File,
    path_for_err: &Path,
    expected: &ExpectedFileContent,
    check_filemode: bool,
) -> GitResult<bool> {
    let meta = file.metadata().map_err(|e| GitError::io(path_for_err, e))?;
    if !meta.file_type().is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    if check_filemode {
        use std::os::unix::fs::PermissionsExt;
        let is_exec = (meta.permissions().mode() & 0o100) != 0;
        let expect_exec = expected.mode == "100755";
        if is_exec != expect_exec {
            return Ok(false);
        }
    }
    #[cfg(not(unix))]
    let _ = check_filemode;

    if meta.len() != expected.len {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| GitError::io(path_for_err, e))?;
    let mut hasher = Sha256Stream::new();
    let total =
        drain_and_hash_reader(file, &mut hasher).map_err(|e| GitError::io(path_for_err, e))?;
    Ok(total == expected.len && hasher.finalize_hex() == expected.sha256)
}

#[cfg(target_os = "linux")]
fn renameat2_exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let ca = CString::new(a.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let cb = CString::new(b.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `ca` and `cb` are valid NUL-terminated C strings passed to the Linux
    // `renameat2` syscall with `AT_FDCWD` and `RENAME_EXCHANGE`.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE as libc::c_uint,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
    let mut items: Vec<(&str, &TreeBlobEntry)> = Vec::with_capacity(expected_tree.len());
    let mut oids: Vec<&str> = Vec::with_capacity(expected_tree.len());
    for (rel, entry) in &expected_tree {
        if !matches!(entry.mode.as_str(), "100644" | "100755" | "120000") {
            return Err(GitError::InvalidMapping(format!(
                "refusing to export into a Git repository with unsupported tracked entry {rel:?} \
                 (mode {})",
                entry.mode
            )));
        }
        items.push((rel.as_str(), entry));
        oids.push(entry.oid.as_str());
    }
    let check_filemode = git_core_filemode(repo_dir);
    stream_git_blobs(repo_dir, &oids, |idx, _, blob_size, blob_reader| {
        let (rel, entry) = items[idx];
        let (matched, _) = stream_compare_and_hash_worktree_entry(
            repo_dir,
            rel,
            entry,
            blob_size,
            blob_reader,
            check_filemode,
        )?;
        if !matched {
            return Err(GitError::InvalidMapping(
                "refusing to export into a Git repository with uncommitted modifications to \
                 tracked files"
                    .into(),
            ));
        }
        Ok(())
    })
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

enum WriteExpectation {
    Absent,
    Existing {
        old: ExpectedFileContent,
        recovery_target: Option<ExpectedFileContent>,
    },
}

struct PlannedWrite {
    rel: String,
    target_mode: String,
    target_oid: String,
    expectation: WriteExpectation,
}

struct PlannedRemoval {
    rel: String,
    expected: ExpectedFileContent,
}

fn stage_blob_into_temp(
    staging_dir: &Path,
    target_mode: &str,
    blob_reader: &mut dyn Read,
) -> GitResult<tempfile::TempPath> {
    let mut tmp = tempfile::Builder::new()
        .prefix("worktree-")
        .tempfile_in(staging_dir)
        .map_err(|e| GitError::io(staging_dir, e))?;
    {
        let mut bw = BufWriter::with_capacity(64 * 1024, tmp.as_file_mut());
        io::copy(blob_reader, &mut bw).map_err(|e| GitError::io(staging_dir, e))?;
        bw.flush().map_err(|e| GitError::io(staging_dir, e))?;
    }
    #[cfg(unix)]
    if target_mode == "100755" {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        tmp.as_file()
            .set_permissions(perms)
            .map_err(|e| GitError::io(tmp.path(), e))?;
    }
    #[cfg(not(unix))]
    let _ = target_mode;
    tmp.as_file()
        .sync_all()
        .map_err(|e| GitError::io(tmp.path(), e))?;
    Ok(tmp.into_temp_path())
}

fn restage_temp_in_dir(
    temp_path: tempfile::TempPath,
    dest_dir: &Path,
    target_mode: &str,
) -> GitResult<tempfile::TempPath> {
    let mut src = File::open(&temp_path).map_err(|e| GitError::io(&temp_path, e))?;
    let next = stage_blob_into_temp(dest_dir, target_mode, &mut src)?;
    drop(temp_path);
    Ok(next)
}

fn install_absent_noclobber(
    temp_path: tempfile::TempPath,
    full: &Path,
    parent: &Path,
    rel: &str,
    target_mode: &str,
) -> GitResult<()> {
    match temp_path.persist_noclobber(full) {
        Ok(_) => {
            crate::fsutil::sync_dir(parent).map_err(|e| GitError::io(parent, e))?;
            Ok(())
        }
        Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => Err(GitError::UnsafePath {
            path: rel.to_string(),
            reason: format!(
                "refusing to overwrite file created concurrently at {rel:?} in repository working \
                 tree"
            ),
        }),
        Err(err) if err.error.kind() == io::ErrorKind::CrossesDevices => {
            let local_tmp = restage_temp_in_dir(err.path, parent, target_mode)?;
            match local_tmp.persist_noclobber(full) {
                Ok(_) => {
                    crate::fsutil::sync_dir(parent).map_err(|e| GitError::io(parent, e))?;
                    Ok(())
                }
                Err(e2) if e2.error.kind() == io::ErrorKind::AlreadyExists => {
                    Err(GitError::UnsafePath {
                        path: rel.to_string(),
                        reason: format!(
                            "refusing to overwrite file created concurrently at {rel:?} in \
                             repository working tree"
                        ),
                    })
                }
                Err(e2) => Err(GitError::io(full, e2.error)),
            }
        }
        Err(err) => Err(GitError::io(full, err.error)),
    }
}

#[allow(clippy::too_many_arguments)]
fn replace_existing_preserving_concurrent_edits(
    mut temp_path: tempfile::TempPath,
    full: &Path,
    parent: &Path,
    rel: &str,
    target_mode: &str,
    old: &ExpectedFileContent,
    recovery_target: Option<&ExpectedFileContent>,
    check_filemode: bool,
) -> GitResult<()> {
    // 1. Mutation-boundary ownership re-check BEFORE touching `full`.
    if let Some(rt) = recovery_target
        && verify_path_matches_expected(full, rel, rt, check_filemode)?
    {
        return Ok(());
    }
    if !verify_path_matches_expected(full, rel, old, check_filemode)? {
        return Err(GitError::InvalidMapping(format!(
            "refusing to overwrite locally modified tracked file {rel:?}"
        )));
    }

    // 2. On Linux, perform an atomic `RENAME_EXCHANGE` swap so any concurrent edit or
    //    atomic replacement that raced with the mutation lands in `temp_path`, where we
    //    verify it and swap back immediately if it changed.
    #[cfg(target_os = "linux")]
    {
        let mut exchanged = renameat2_exchange(temp_path.as_ref(), full);
        if let Err(ref e) = exchanged
            && e.kind() == io::ErrorKind::CrossesDevices
        {
            temp_path = restage_temp_in_dir(temp_path, parent, target_mode)?;
            exchanged = renameat2_exchange(temp_path.as_ref(), full);
        }
        match exchanged {
            Ok(()) => {
                let swapped_ok =
                    verify_path_matches_expected(temp_path.as_ref(), rel, old, check_filemode)?
                        || recovery_target.is_some_and(|rt| {
                            verify_path_matches_expected(
                                temp_path.as_ref(),
                                rel,
                                rt,
                                check_filemode,
                            )
                            .unwrap_or(false)
                        });
                if !swapped_ok {
                    // Intervening edit or replacement detected right at the mutation boundary:
                    // atomically restore the user's file back to `full` and refuse.
                    let _ = renameat2_exchange(temp_path.as_ref(), full);
                    let _ = crate::fsutil::sync_dir(parent);
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to overwrite concurrently modified tracked file {rel:?}"
                    )));
                }
                drop(temp_path);
                crate::fsutil::sync_dir(parent).map_err(|e| GitError::io(parent, e))?;
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(GitError::InvalidMapping(format!(
                    "refusing worktree sync: tracked file {rel:?} disappeared concurrently"
                )));
            }
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
                ) =>
            {
                // Filesystem does not support RENAME_EXCHANGE; fall through to open-fd fallback.
            }
            Err(e) => return Err(GitError::io(full, e)),
        }
    }

    // 3. Portable fallback: hold an open fd on the original inode across rename, and re-check
    //    its contents immediately after rename to detect and restore any in-place write.
    let (mut orig_fd, _) = open_regular_file_no_symlink(full, rel)?;
    if !verify_open_file_matches_expected(&mut orig_fd, full, old, check_filemode)? {
        return Err(GitError::InvalidMapping(format!(
            "refusing to overwrite locally modified tracked file {rel:?}"
        )));
    }
    match temp_path.persist(full) {
        Ok(_) => {}
        Err(err) if err.error.kind() == io::ErrorKind::CrossesDevices => {
            let local_tmp = restage_temp_in_dir(err.path, parent, target_mode)?;
            local_tmp
                .persist(full)
                .map_err(|e| GitError::io(full, e.error))?;
        }
        Err(err) => return Err(GitError::io(full, err.error)),
    }
    if !verify_open_file_matches_expected(&mut orig_fd, full, old, check_filemode)? {
        orig_fd
            .seek(SeekFrom::Start(0))
            .map_err(|e| GitError::io(full, e))?;
        let restored = stage_blob_into_temp(parent, &old.mode, &mut orig_fd)?;
        let _ = restored.persist(full);
        let _ = crate::fsutil::sync_dir(parent);
        return Err(GitError::InvalidMapping(format!(
            "refusing to overwrite concurrently modified tracked file {rel:?}"
        )));
    }
    crate::fsutil::sync_dir(parent).map_err(|e| GitError::io(parent, e))?;
    Ok(())
}

fn remove_verified_preserving_concurrent_edits(
    full: &Path,
    staging_dir: &Path,
    rel: &str,
    expected: &ExpectedFileContent,
    mode: SyncMode,
    check_filemode: bool,
) -> GitResult<bool> {
    match fs::symlink_metadata(full) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if mode == SyncMode::Recovery {
                return Ok(false);
            }
            return Err(GitError::InvalidMapping(format!(
                "refusing to remove missing tracked file {rel:?}"
            )));
        }
        Err(e) => return Err(GitError::io(full, e)),
        Ok(_) => {}
    }

    // 1. Mutation-boundary ownership re-check BEFORE moving or unlinking `full`.
    if !verify_path_matches_expected(full, rel, expected, check_filemode)? {
        return Err(GitError::InvalidMapping(format!(
            "refusing to remove locally modified tracked file {rel:?}"
        )));
    }

    // 2. Atomically move `full` out of the worktree into `.git/sparkles/staging/`, re-verify
    //    the moved entry still matches `expected`, and restore it immediately if an
    //    intervening edit raced with the move.
    let trash_path = staging_dir.join(format!("remove-{}", Uuid::new_v4()));
    match fs::rename(full, &trash_path) {
        Ok(()) => {
            let still_matches =
                verify_path_matches_expected(&trash_path, rel, expected, check_filemode)?;
            if !still_matches {
                // Restore the concurrently edited file back to `full` without clobbering any
                // newly created file at `full`.
                let restored = fs::hard_link(&trash_path, full)
                    .map(|()| {
                        let _ = fs::remove_file(&trash_path);
                    })
                    .or_else(|_| fs::rename(&trash_path, full));
                let _ = restored;
                if let Some(p) = full.parent() {
                    let _ = crate::fsutil::sync_dir(p);
                }
                return Err(GitError::InvalidMapping(format!(
                    "refusing to remove concurrently modified tracked file {rel:?}"
                )));
            }
            match fs::remove_file(&trash_path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(GitError::io(&trash_path, e)),
            }
            if let Some(p) = full.parent() {
                let _ = crate::fsutil::sync_dir(p);
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound && mode == SyncMode::Recovery => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            // Different mount: verify once more and unlink.
            if !verify_path_matches_expected(full, rel, expected, check_filemode)? {
                return Err(GitError::InvalidMapping(format!(
                    "refusing to remove locally modified tracked file {rel:?}"
                )));
            }
            fs::remove_file(full).map_err(|err| GitError::io(full, err))?;
            if let Some(p) = full.parent() {
                let _ = crate::fsutil::sync_dir(p);
            }
            Ok(true)
        }
        Err(e) => Err(GitError::io(full, e)),
    }
}

/// Synchronize `HEAD`, `.git/index`, and `clean_repo`'s working tree to `commit_sha` on
/// `git_branch` while holding `.git/index.lock`.
///
/// Verifies `.git/index` and every affected working-tree path against raw Git blobs (never
/// running `.gitattributes` clean or smudge filters) via streaming `cat-file --batch`,
/// enforces mutation-boundary ownership (no-clobber install for absent targets, atomic
/// exchange/restore for existing targets and removals), updates `HEAD`, and atomically swaps
/// `.git/index` while holding `.git/index.lock`.
#[allow(clippy::too_many_arguments)]
fn sync_worktree_to_commit_no_filters(
    clean_repo: &Path,
    git_dir: &Path,
    git_branch: &str,
    previous_commit_sha: Option<&str>,
    prev_head_ref: Option<&str>,
    commit_sha: &str,
    lock_token: &str,
    mode: SyncMode,
) -> GitResult<()> {
    ensure_no_in_progress_git_op(git_dir)?;
    let index_lock = GitIndexLock::acquire(git_dir, lock_token, mode == SyncMode::Recovery)?;

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

    let check_filemode = git_core_filemode(clean_repo);

    // Post-sync recovery: HEAD and index already point to `commit_sha`; stream-verify every
    // target file against its raw blob bytes so local post-sync edits are never ignored, then
    // finish cleanly (releasing any reclaimed stale `.git/index.lock` on drop).
    if mode == SyncMode::Recovery
        && index_matches_new
        && head_sym.as_deref() == Some(ref_name.as_str())
    {
        let target_items: Vec<(&str, &TreeBlobEntry)> =
            target_tree.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let target_oids: Vec<&str> = target_items.iter().map(|(_, e)| e.oid.as_str()).collect();
        stream_git_blobs(
            clean_repo,
            &target_oids,
            |idx, _, blob_size, blob_reader| {
                let (rel, target_entry) = target_items[idx];
                let (matched, _) = stream_compare_and_hash_worktree_entry(
                    clean_repo,
                    rel,
                    target_entry,
                    blob_size,
                    blob_reader,
                    check_filemode,
                )?;
                if !matched {
                    return Err(GitError::InvalidMapping(
                        "refusing pending recovery with uncommitted modifications to tracked files"
                            .into(),
                    ));
                }
                Ok(())
            },
        )?;
        return Ok(());
    }

    // Pass 1: Read-only streaming preflight of every target path (checking existence, type,
    // and whether on-disk bytes already match `target_entry`).
    struct TargetPreflight {
        rel: String,
        target_entry: TreeBlobEntry,
        exists: bool,
        matches_new: bool,
        target_expected: ExpectedFileContent,
    }
    let mut target_states: Vec<TargetPreflight> = Vec::with_capacity(target_tree.len());
    let mut need_target_check: Vec<(&str, &TreeBlobEntry)> = Vec::new();
    for (rel, target_entry) in &target_tree {
        let full = resolve_safe_path(clean_repo, rel)?;
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if old_tree.contains_key(rel) {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing worktree sync: previously tracked file {rel:?} was locally \
                         deleted"
                    )));
                }
                target_states.push(TargetPreflight {
                    rel: rel.clone(),
                    target_entry: target_entry.clone(),
                    exists: false,
                    matches_new: false,
                    target_expected: ExpectedFileContent {
                        mode: target_entry.mode.clone(),
                        len: 0,
                        sha256: String::new(),
                    },
                });
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
                if mode == SyncMode::Normal && !old_tree.contains_key(rel) {
                    return Err(GitError::UnsafePath {
                        path: rel.clone(),
                        reason: format!(
                            "refusing to overwrite unmanaged file {rel:?} in repository working \
                             tree"
                        ),
                    });
                }
                need_target_check.push((rel.as_str(), target_entry));
            }
        }
    }
    if !need_target_check.is_empty() {
        let target_oids: Vec<&str> = need_target_check
            .iter()
            .map(|(_, e)| e.oid.as_str())
            .collect();
        stream_git_blobs(
            clean_repo,
            &target_oids,
            |idx, _, blob_size, blob_reader| {
                let (rel, target_entry) = need_target_check[idx];
                let (matches_new, sha256) = stream_compare_and_hash_worktree_entry(
                    clean_repo,
                    rel,
                    target_entry,
                    blob_size,
                    blob_reader,
                    check_filemode,
                )?;
                target_states.push(TargetPreflight {
                    rel: rel.to_string(),
                    target_entry: target_entry.clone(),
                    exists: true,
                    matches_new,
                    target_expected: ExpectedFileContent {
                        mode: target_entry.mode.clone(),
                        len: blob_size,
                        sha256,
                    },
                });
                Ok(())
            },
        )?;
    }

    // Pass 2: Read-only streaming preflight against `old_tree` for every existing target file
    // that needs old-blob verification AND every stale tracked path in `old_tree \ target_tree`.
    let mut old_checks: Vec<(
        String,
        TreeBlobEntry,
        bool,
        Option<ExpectedFileContent>,
        bool,
    )> = Vec::new();
    // Tuple: (rel, old_entry, is_target_overwrite, recovery_target, target_matches_new)
    let mut to_write: Vec<PlannedWrite> = Vec::new();

    for ts in target_states {
        if !ts.exists {
            to_write.push(PlannedWrite {
                rel: ts.rel,
                target_mode: ts.target_entry.mode,
                target_oid: ts.target_entry.oid,
                expectation: WriteExpectation::Absent,
            });
            continue;
        }
        match mode {
            SyncMode::Normal => {
                let Some(old_entry) = old_tree.get(&ts.rel) else {
                    return Err(GitError::UnsafePath {
                        path: ts.rel.clone(),
                        reason: format!(
                            "refusing to overwrite unmanaged file {:?} in repository working tree",
                            ts.rel
                        ),
                    });
                };
                if !matches!(old_entry.mode.as_str(), "100644" | "100755" | "120000") {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to overwrite unsupported tracked entry {:?} (mode {})",
                        ts.rel, old_entry.mode
                    )));
                }
                old_checks.push((ts.rel, old_entry.clone(), true, None, ts.matches_new));
            }
            SyncMode::Recovery => {
                if ts.matches_new {
                    // Already written by the interrupted sync (or unchanged).
                } else if let Some(old_entry) = old_tree.get(&ts.rel) {
                    if !matches!(old_entry.mode.as_str(), "100644" | "100755" | "120000") {
                        return Err(GitError::InvalidMapping(format!(
                            "refusing pending recovery on unsupported tracked entry {:?} (mode {})",
                            ts.rel, old_entry.mode
                        )));
                    }
                    old_checks.push((
                        ts.rel,
                        old_entry.clone(),
                        true,
                        Some(ts.target_expected),
                        false,
                    ));
                } else {
                    return Err(GitError::UnsafePath {
                        path: ts.rel.clone(),
                        reason: format!(
                            "refusing to overwrite unmanaged file {:?} in repository working tree",
                            ts.rel
                        ),
                    });
                }
            }
        }
    }

    for (old_rel, old_entry) in &old_tree {
        if target_tree.contains_key(old_rel) {
            continue;
        }
        validate_relative_path(old_rel)?;
        let is_tracked_symlink = old_entry.mode == "120000";
        let full = if is_tracked_symlink {
            resolve_safe_path_allow_leaf_symlink(clean_repo, old_rel)?
        } else {
            resolve_safe_path(clean_repo, old_rel)?
        };
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
                if meta.file_type().is_symlink() && !is_tracked_symlink {
                    return Err(GitError::UnsafePath {
                        path: old_rel.clone(),
                        reason: "tracked path in repository must not be a symlink".into(),
                    });
                }
                if !matches!(old_entry.mode.as_str(), "100644" | "100755" | "120000") {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to remove unsupported tracked entry {old_rel:?} (mode {})",
                        old_entry.mode
                    )));
                }
                old_checks.push((old_rel.clone(), old_entry.clone(), false, None, false));
            }
        }
    }

    let mut to_remove: Vec<PlannedRemoval> = Vec::new();
    if !old_checks.is_empty() {
        let old_oids: Vec<&str> = old_checks
            .iter()
            .map(|(_, e, _, _, _)| e.oid.as_str())
            .collect();
        stream_git_blobs(clean_repo, &old_oids, |idx, _, blob_size, blob_reader| {
            let (ref rel, ref old_entry, is_target_overwrite, ref recovery_target, matches_new) =
                old_checks[idx];
            let (matches_old, sha256) = stream_compare_and_hash_worktree_entry(
                clean_repo,
                rel,
                old_entry,
                blob_size,
                blob_reader,
                check_filemode,
            )?;
            let old_expected = ExpectedFileContent {
                mode: old_entry.mode.clone(),
                len: blob_size,
                sha256,
            };
            if is_target_overwrite {
                if !matches_old {
                    return Err(GitError::InvalidMapping(match mode {
                        SyncMode::Normal => {
                            format!("refusing to overwrite locally modified tracked file {rel:?}")
                        }
                        SyncMode::Recovery => format!(
                            "refusing pending recovery: tracked file {rel:?} matches neither \
                             pre-sync nor post-sync state"
                        ),
                    }));
                }
                if !matches_new {
                    let target_entry = &target_tree[rel];
                    to_write.push(PlannedWrite {
                        rel: rel.clone(),
                        target_mode: target_entry.mode.clone(),
                        target_oid: target_entry.oid.clone(),
                        expectation: WriteExpectation::Existing {
                            old: old_expected,
                            recovery_target: recovery_target.clone(),
                        },
                    });
                }
            } else {
                if !matches_old {
                    return Err(GitError::InvalidMapping(format!(
                        "refusing to remove locally modified tracked file {rel:?}"
                    )));
                }
                to_remove.push(PlannedRemoval {
                    rel: rel.clone(),
                    expected: old_expected,
                });
            }
            Ok(())
        })?;
    }

    #[cfg(test)]
    test_hooks::before_worktree_mutations(clean_repo);

    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let staging_parent = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_STAGING_SUBDIR)?;

    // Stream and install every changed tracked blob byte-exact without Git checkout/smudge
    // filters, checking ownership at each mutation boundary.
    if !to_write.is_empty() {
        let write_oids: Vec<&str> = to_write.iter().map(|w| w.target_oid.as_str()).collect();
        stream_git_blobs(clean_repo, &write_oids, |idx, _, _, blob_reader| {
            let planned = &to_write[idx];
            let full = resolve_safe_path(clean_repo, &planned.rel)?;
            let parent = effective_parent(&full)?;
            ensure_dir_no_symlink(&parent)?;
            let temp_path =
                stage_blob_into_temp(&staging_parent, &planned.target_mode, blob_reader)?;
            match &planned.expectation {
                WriteExpectation::Absent => {
                    install_absent_noclobber(
                        temp_path,
                        &full,
                        &parent,
                        &planned.rel,
                        &planned.target_mode,
                    )?;
                }
                WriteExpectation::Existing {
                    old,
                    recovery_target,
                } => {
                    replace_existing_preserving_concurrent_edits(
                        temp_path,
                        &full,
                        &parent,
                        &planned.rel,
                        &planned.target_mode,
                        old,
                        recovery_target.as_ref(),
                        check_filemode,
                    )?;
                }
            }
            #[cfg(test)]
            if idx == 0 {
                test_hooks::after_first_worktree_blob_write()?;
            }
            Ok(())
        })?;
    }

    // Remove verified previously tracked files absent from the target commit, checking
    // ownership at each mutation boundary and preserving any concurrent edit.
    let mut removed_count = 0usize;
    for planned in to_remove {
        let is_tracked_symlink = planned.expected.mode == "120000";
        let full = if is_tracked_symlink {
            resolve_safe_path_allow_leaf_symlink(clean_repo, &planned.rel)?
        } else {
            resolve_safe_path(clean_repo, &planned.rel)?
        };
        if remove_verified_preserving_concurrent_edits(
            &full,
            &staging_parent,
            &planned.rel,
            &planned.expected,
            mode,
            check_filemode,
        )? {
            removed_count += 1;
            #[cfg(test)]
            if removed_count == 1 {
                test_hooks::after_first_worktree_removal()?;
            }
        }
    }
    let _ = removed_count;

    run_git(clean_repo, &["symbolic-ref", "HEAD", &ref_name], &[])?;
    #[cfg(test)]
    test_hooks::after_symbolic_ref_before_index()?;

    // Build the exact target index in an isolated temporary file from the object database
    // (never running filters) and atomically swap it into `.git/index` while holding our
    // ownership-proven `.git/index.lock`.
    let tmp_index = tempfile::Builder::new()
        .prefix("sync-index-")
        .tempfile_in(&staging_parent)
        .map_err(|e| GitError::io(&staging_parent, e))?;
    let tmp_index_path = tmp_index.into_temp_path();
    let _ = fs::remove_file(&tmp_index_path);
    let tmp_index_str = tmp_index_path.to_string_lossy().into_owned();
    let idx_env = [("GIT_INDEX_FILE", tmp_index_str.as_str())];
    run_git(clean_repo, &["read-tree", commit_sha], &idx_env)?;
    index_lock.commit_index_from_path(git_dir, &tmp_index_path)?;
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
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let (write_res, stdout_res, stderr_res) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> io::Result<()> {
            if let Some(mut s) = stdin
                && let Err(e) = s.write_all(stdin_bytes)
                && e.kind() != io::ErrorKind::BrokenPipe
            {
                return Err(e);
            }
            Ok(())
        });
        let err_reader = scope.spawn(move || -> io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            stderr.read_to_end(&mut buf)?;
            Ok(buf)
        });
        let mut out_buf = Vec::new();
        let stdout_res = stdout.read_to_end(&mut out_buf).map(|_| out_buf);
        let write_res = writer
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("git stdin writer panicked")));
        let stderr_res = err_reader
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("git stderr reader panicked")));
        (write_res, stdout_res, stderr_res)
    });

    let status = child.wait().map_err(|e| GitError::GitCommand {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        status: None,
        stderr: e.to_string(),
    })?;
    let stderr_bytes = stderr_res.unwrap_or_default();
    if let Err(e) = write_res {
        return Err(GitError::GitCommand {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: status.code(),
            stderr: e.to_string(),
        });
    }
    let stdout_bytes = stdout_res.map_err(|e| GitError::GitCommand {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        status: status.code(),
        stderr: e.to_string(),
    })?;
    if !status.success() {
        return Err(GitError::GitCommand {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        });
    }
    Ok(stdout_bytes)
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
        pub exit_after_first_blob_write: Option<i32>,
        pub fail_after_first_removal: bool,
        pub fail_after_symbolic_ref: bool,
        pub fail_before_remove_pending: bool,
        pub inject_concurrent_stage: Option<(String, Vec<u8>)>,
        pub inject_stage_before_sync: Option<(String, Vec<u8>)>,
        pub inject_tracked_edit_after_clean: Option<(String, Vec<u8>)>,
        pub inject_tracked_edit_before_sync: Option<(String, Vec<u8>)>,
        pub inject_edit_after_preflight: Option<(String, Vec<u8>)>,
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

    pub(super) fn before_worktree_mutations(repo_dir: &Path) {
        let edit = FAULTS.with(|f| f.borrow_mut().inject_edit_after_preflight.take());
        if let Some((rel, bytes)) = edit {
            let p = repo_dir.join(&rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, bytes).unwrap();
        }
    }

    pub(super) fn after_first_worktree_blob_write() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if let Some(code) = s.exit_after_first_blob_write.take() {
                std::process::exit(code);
            }
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
            format_version: PENDING_PUBLICATION_FORMAT_VERSION,
            index_lock_token: Uuid::new_v4().to_string(),
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

    #[test]
    fn abrupt_child_exit_holding_index_lock_recovers_while_foreign_lock_is_refused() {
        const CHILD_DB_ENV: &str = "SPARKLES_TEST_EXPORT_CRASH_DB";
        const CHILD_REPO_ENV: &str = "SPARKLES_TEST_EXPORT_CRASH_REPO";

        if let (Ok(db_p), Ok(repo_p)) = (std::env::var(CHILD_DB_ENV), std::env::var(CHILD_REPO_ENV))
        {
            let ds = Dataset::open(&db_p).unwrap();
            let _g = test_hooks::install(test_hooks::FaultState {
                exit_after_first_blob_write: Some(99),
                ..Default::default()
            });
            let _ = export_branch_to_git(&ds, Path::new(&repo_p), &GitExportOptions::default());
            std::process::exit(1);
        }

        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("db");
        {
            let ds = Dataset::open(&db_path).unwrap();
            ds.transaction(|tx| {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked("http://example.org/s1"),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(1i64),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked("http://example.org/s2"),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(2i64),
                        GraphName::NamedNode(NamedNode::new_unchecked("http://example.org/g2")),
                    )
                    .as_ref(),
                )?;
                Ok(())
            })
            .unwrap();
        }
        let repo = tempfile::tempdir().unwrap();

        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::export::tests::abrupt_child_exit_holding_index_lock_recovers_while_foreign_lock_is_refused",
                "--nocapture",
            ])
            .env(CHILD_DB_ENV, &db_path)
            .env(CHILD_REPO_ENV, repo.path())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(99), "child must exit abruptly with 99");

        let index_lock_path = repo.path().join(".git/index.lock");
        assert!(
            index_lock_path.is_file(),
            "abrupt exit must leave .git/index.lock on disk"
        );
        let own_lock_bytes = fs::read(&index_lock_path).unwrap();
        assert!(
            own_lock_bytes.starts_with(INDEX_LOCK_PREFIX.as_bytes()),
            "index.lock must carry Sparkles ownership prefix"
        );

        let ds = Dataset::open(&db_path).unwrap();

        // 1. Foreign `.git/index.lock` (wrong bytes) must be refused and preserved.
        fs::write(&index_lock_path, b"DIRC-foreign-git-lock").unwrap();
        let err_foreign =
            export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
        assert!(
            err_foreign.to_string().contains("index.lock"),
            "expected foreign lock refusal, got {err_foreign}"
        );
        assert_eq!(
            fs::read(&index_lock_path).unwrap(),
            b"DIRC-foreign-git-lock"
        );

        // 2. Empty `.git/index.lock` (e.g. brand-new foreign git process) must be refused.
        fs::write(&index_lock_path, b"").unwrap();
        assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        assert!(index_lock_path.is_file());

        // 3. Restore our exact ownership token; while an active OS lock is held on it,
        //    recovery must still refuse.
        fs::write(&index_lock_path, &own_lock_bytes).unwrap();
        {
            let active = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&index_lock_path)
                .unwrap();
            active.lock().unwrap();
            let err_active =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(
                err_active.to_string().contains("index.lock"),
                "expected active lock refusal, got {err_active}"
            );
        }

        // 4. Once no process holds the OS lock, recovery reclaims our stale lock and succeeds.
        let report = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert_eq!(report.mapping.entries.len(), 1);
        assert!(
            !index_lock_path.exists(),
            "stale index.lock must be removed"
        );
        verify_git_tree(repo.path()).unwrap();
    }

    #[test]
    fn edits_and_new_files_after_checkout_preflight_are_preserved() {
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        let insert_quad = |s: &str, g: Option<&str>| {
            ds.transaction(|tx| {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(s),
                        NamedNode::new_unchecked("http://example.org/p"),
                        Literal::from(s),
                        match g {
                            Some(iri) => GraphName::NamedNode(NamedNode::new_unchecked(iri)),
                            None => GraphName::DefaultGraph,
                        },
                    )
                    .as_ref(),
                )?;
                Ok(())
            })
            .unwrap();
        };

        insert_quad("http://example.org/s1", None);
        insert_quad(
            "http://example.org/drop1",
            Some("http://example.org/drop-me"),
        );
        let repo = tempfile::tempdir().unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();

        // Find the dropped graph path on disk.
        let drop_rel = fs::read_dir(repo.path().join(GRAPHS_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .find(|n| n.contains("drop-me"))
            .map(|n| format!("{GRAPHS_DIR}/{n}"))
            .unwrap();

        // Commit 2 modifies `graphs/default.ttl`.
        insert_quad("http://example.org/s2", None);

        // A. Edit an existing tracked file (`graphs/default.ttl`) AFTER eligibility preflight
        //    and BEFORE mutation -> must refuse and preserve the user's edit.
        let orig_default = fs::read(repo.path().join("graphs/default.ttl")).unwrap();
        let user_edit = b"# edited after preflight\n";
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_edit_after_preflight: Some((
                    "graphs/default.ttl".into(),
                    user_edit.to_vec(),
                )),
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        assert_eq!(
            fs::read(repo.path().join("graphs/default.ttl")).unwrap(),
            user_edit,
            "intervening edit to existing tracked file must be preserved"
        );
        fs::write(repo.path().join("graphs/default.ttl"), &orig_default).unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();

        // Commit 3 adds a brand-new named graph (`brand-new`) that is absent from the worktree.
        insert_quad(
            "http://example.org/ng",
            Some("http://example.org/brand-new"),
        );
        // Stop right before worktree sync so `refs/heads/main` names the exact new graph path
        // while the worktree has NOT yet written it.
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                fail_worktree_sync: true,
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        let tip_files = run_git(
            repo.path(),
            &["ls-tree", "-r", "--name-only", "refs/heads/main"],
            &[],
        )
        .unwrap();
        let new_graph_rel = tip_files
            .lines()
            .find(|l| l.contains("brand-new"))
            .unwrap()
            .to_string();
        assert!(!repo.path().join(&new_graph_rel).exists());

        // B. Create a brand-new regular file at previously absent `new_graph_rel` AFTER
        //    eligibility preflight -> no-clobber install must refuse and preserve its bytes!
        let concurrent_new = b"# user created file concurrently\n";
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_edit_after_preflight: Some((new_graph_rel.clone(), concurrent_new.to_vec())),
                ..Default::default()
            });
            let err =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
            assert!(
                matches!(err, GitError::UnsafePath { .. }),
                "expected UnsafePath on no-clobber collision, got {err:?}"
            );
        }
        assert_eq!(
            fs::read(repo.path().join(&new_graph_rel)).unwrap(),
            concurrent_new,
            "concurrently created file at absent target must not be clobbered"
        );
        fs::remove_file(repo.path().join(&new_graph_rel)).unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();

        // Commit 4 drops `drop-me`.
        let orig_drop = fs::read(repo.path().join(&drop_rel)).unwrap();
        ds.update("DROP GRAPH <http://example.org/drop-me>")
            .unwrap();

        // C. Edit stale tracked file in `to_remove` AFTER eligibility preflight and BEFORE
        //    removal -> must refuse and preserve the edited file on disk!
        let edit_drop = b"# user saved notes in file being removed\n";
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_edit_after_preflight: Some((drop_rel.clone(), edit_drop.to_vec())),
                ..Default::default()
            });
            assert!(export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).is_err());
        }
        assert_eq!(
            fs::read(repo.path().join(&drop_rel)).unwrap(),
            edit_drop,
            "intervening edit to file being removed must be preserved"
        );
        fs::write(repo.path().join(&drop_rel), orig_drop).unwrap();
        export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        assert!(!repo.path().join(&drop_rel).exists());
        verify_git_tree(repo.path()).unwrap();
    }

    #[test]
    fn large_git_blob_batch_exceeding_pipe_buffers_does_not_deadlock() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q", "-b", "seed"], &[]).unwrap();
        run_git(repo.path(), &["config", "user.name", "Test"], &[]).unwrap();
        run_git(repo.path(), &["config", "user.email", "t@example.org"], &[]).unwrap();

        // Write 2,200 files (> 90 KiB of OID lines on stdin, exceeding 64 KiB Linux pipe
        // buffer), with the first file > 256 KiB (exceeding 64 KiB stdout pipe buffer).
        let big_body = vec![b'A'; 256 * 1024];
        let dir_files = repo.path().join("bulk");
        fs::create_dir_all(&dir_files).unwrap();
        fs::write(dir_files.join("0000.txt"), &big_body).unwrap();
        for i in 1..2200usize {
            fs::write(
                dir_files.join(format!("{i:04}.txt")),
                format!("entry-{i}\n"),
            )
            .unwrap();
        }
        run_git(repo.path(), &["add", "."], &[]).unwrap();
        run_git(repo.path(), &["commit", "-q", "-m", "seed 2200 files"], &[]).unwrap();

        let git_dir = resolve_exact_git_dir(repo.path()).unwrap();
        ensure_clean_git_state(repo.path(), &git_dir).unwrap();

        // Also export a Sparkles dataset onto a separate branch, exercising both clean check
        // and streaming checkout removal across all 2,200 old tracked blobs.
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        ds.transaction(|tx| {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked("http://example.org/s"),
                    NamedNode::new_unchecked("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        })
        .unwrap();
        export_branch_to_git(
            &ds,
            repo.path(),
            &GitExportOptions {
                git_branch: Some("sparkles".into()),
                ..GitExportOptions::default()
            },
        )
        .unwrap();
        verify_git_tree(repo.path()).unwrap();
    }

    #[test]
    fn pending_publication_rejects_legacy_v1_and_unknown_versions_before_replay() {
        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        ds.transaction(|tx| {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked("http://example.org/s1"),
                    NamedNode::new_unchecked("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        })
        .unwrap();

        let repo = tempfile::tempdir().unwrap();
        let r1 = export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
        let git_dir = resolve_exact_git_dir(repo.path()).unwrap();
        let pending_path = pending_publication_path(&git_dir, "main").unwrap();

        // Legacy v1 journal lacking `prev_checkout_sha` / `index_lock_token`.
        let legacy_v1 = serde_json::json!({
            "format_version": 1,
            "mapping_path": r1.mapping_path,
            "ref_name": "refs/heads/main",
            "old_ref_sha": r1.head_git_sha,
            "new_commit_sha": r1.head_git_sha,
            "mapping": r1.mapping,
        });
        fs::write(&pending_path, serde_json::to_vec(&legacy_v1).unwrap()).unwrap();
        let err_v1 =
            export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
        assert!(
            matches!(
                err_v1,
                GitError::UnsupportedFormatVersion {
                    found: 1,
                    supported: PENDING_PUBLICATION_FORMAT_VERSION,
                    ..
                }
            ),
            "expected UnsupportedFormatVersion for legacy v1 journal, got {err_v1:?}"
        );

        // Future v99 journal.
        fs::write(&pending_path, br#"{"format_version":99}"#).unwrap();
        let err_v99 =
            export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap_err();
        assert!(
            matches!(
                err_v99,
                GitError::UnsupportedFormatVersion {
                    found: 99,
                    supported: PENDING_PUBLICATION_FORMAT_VERSION,
                    ..
                }
            ),
            "expected UnsupportedFormatVersion for future v99 journal, got {err_v99:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlink_and_owner_execute_bit_checks_match_git_semantics() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q", "-b", "seed"], &[]).unwrap();
        run_git(repo.path(), &["config", "user.name", "Test"], &[]).unwrap();
        run_git(repo.path(), &["config", "user.email", "t@example.org"], &[]).unwrap();
        run_git(repo.path(), &["config", "core.fileMode", "true"], &[]).unwrap();

        let readme = repo.path().join("README.md");
        fs::write(&readme, b"hello\n").unwrap();
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o644)).unwrap();
        let link = repo.path().join("link.txt");
        symlink("README.md", &link).unwrap();
        run_git(repo.path(), &["add", "README.md", "link.txt"], &[]).unwrap();
        run_git(repo.path(), &["commit", "-q", "-m", "track symlink"], &[]).unwrap();

        let git_dir = resolve_exact_git_dir(repo.path()).unwrap();

        // 1. Clean tracked 120000 symlink passes clean check.
        ensure_clean_git_state(repo.path(), &git_dir).unwrap();

        // 2. Group/other execute bit (`0o641`) on a `100644` file is clean under Git's `0o100`
        //    rule, whereas setting owner execute (`0o744`) is detected as modified.
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o641)).unwrap();
        ensure_clean_git_state(repo.path(), &git_dir).unwrap();
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o744)).unwrap();
        assert!(ensure_clean_git_state(repo.path(), &git_dir).is_err());
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o644)).unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(db_dir.path().join("db")).unwrap();
        ds.transaction(|tx| {
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked("http://example.org/s"),
                    NamedNode::new_unchecked("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        })
        .unwrap();

        // 3. Modified tracked symlink target is detected and refused as a local modification.
        fs::remove_file(&link).unwrap();
        symlink("OTHER.md", &link).unwrap();
        assert!(
            export_branch_to_git(
                &ds,
                repo.path(),
                &GitExportOptions {
                    git_branch: Some("sparkles".into()),
                    ..GitExportOptions::default()
                },
            )
            .is_err()
        );
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("OTHER.md"));

        // 4. Restoring the tracked symlink allows clean export and unlinks the stale symlink.
        fs::remove_file(&link).unwrap();
        symlink("README.md", &link).unwrap();
        export_branch_to_git(
            &ds,
            repo.path(),
            &GitExportOptions {
                git_branch: Some("sparkles".into()),
                ..GitExportOptions::default()
            },
        )
        .unwrap();
        assert!(fs::symlink_metadata(&link).is_err());
        verify_git_tree(repo.path()).unwrap();
    }
}
