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
    CanonicalSyntax, DEFAULT_MAX_MANIFEST_BYTES, MANIFEST_FORMAT_VERSION, MANIFEST_REL_PATH,
    WriteOptions, check_dest_file_not_symlink, effective_parent, ensure_child_dir_no_symlink,
    ensure_dir_no_symlink, normalize_dir_for_lstat, read_bounded_regular_file,
    reject_duplicate_json_keys, resolve_safe_path, sanitize_slug, sha256_hex,
    validate_relative_path, verify_existing_dir_no_symlink, write_atomic_safe,
    write_snapshot_git_tree_tracked,
};
use crate::git::{GitError, GitResult};
use crate::history::{At, HistoryOptions};
use crate::store::{BranchCommit, Snapshot, Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
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
        let label = path.display().to_string();
        let bytes = read_bounded_regular_file(path, &label, max_bytes)?;
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

/// Crash-recoverable publication intent written before moving a Git ref so a failure or
/// interruption on either side of `git update-ref` can be cleanly reconciled on retry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPublication {
    format_version: u32,
    mapping_path: PathBuf,
    ref_name: String,
    old_ref_sha: Option<String>,
    new_commit_sha: String,
    mapping: CommitMapping,
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

    // Serialize concurrent exports to the same repository.
    let _repo_lock = acquire_export_lock(&git_dir)?;

    // Refuse any in-progress Git merge/rebase/cherry-pick/revert/bisect before touching
    // pending recovery or refs.
    ensure_no_in_progress_git_op(&git_dir)?;

    let mapping_path = match &options.mapping_path {
        Some(p) => {
            prepare_mapping_path_safe(p)?;
            p.clone()
        }
        None => {
            let sparkles_dir = ensure_child_dir_no_symlink(&git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
            let mappings_dir = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_MAPPINGS_SUBDIR)?;
            let slug = sanitize_slug(&git_branch);
            let hash = sha256_hex(git_branch.as_bytes());
            mappings_dir.join(format!("{slug}-{}.json", &hash[..16]))
        }
    };

    let ref_name = format!("refs/heads/{git_branch}");
    let pending_path = pending_publication_path(&git_dir, &git_branch)?;

    // Reconcile any interrupted previous publication on this branch before checking clean
    // repository state, so a failure on either side of `git update-ref` recovers cleanly
    // without manual resets or duplicate commits.
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
    // tree are checked out to `git_branch` byte-exact without `.gitattributes` filters.
    if exported_count == 0 {
        let prev_head = git_rev_parse_opt(&clean_repo, "HEAD")?;
        sync_worktree_to_commit_no_filters(
            &clean_repo,
            &git_branch,
            prev_head.as_deref(),
            &head_git_sha,
        )?;
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
    // 1. Write the canonical profile tree inside an isolated temporary staging directory so
    //    any canonicalization, format, or size error leaves `clean_repo` untouched.
    let sparkles_dir = ensure_child_dir_no_symlink(git_dir, DEFAULT_SPARKLES_GIT_SUBDIR)?;
    let staging_parent = ensure_child_dir_no_symlink(&sparkles_dir, DEFAULT_STAGING_SUBDIR)?;
    let stage_dir = tempfile::Builder::new()
        .prefix("tree-")
        .tempdir_in(&staging_parent)
        .map_err(|e| GitError::io(&staging_parent, e))?;

    let (manifest, _) =
        write_snapshot_git_tree_tracked(snapshot, branch_prefixes, stage_dir.path(), write_opts)?;

    // 2. Preflight the repository working tree BEFORE publishing any Git commit so an
    //    unmanaged file in `clean_repo` is never overwritten or left colliding after ref
    //    publication.
    let mut new_paths: Vec<&str> = Vec::with_capacity(1 + manifest.graphs.len());
    new_paths.push(MANIFEST_REL_PATH);
    for entry in &manifest.graphs {
        new_paths.push(entry.path.as_str());
    }
    let prev_head_sha = git_rev_parse_opt(clean_repo, "HEAD")?;
    let old_tracked = list_tracked_paths_in_commit(clean_repo, prev_head_sha.as_deref())?;
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

    // 5. Serialize and size-check the updated mapping BEFORE writing the pending journal or
    //    advancing the Git ref so an over-limit mapping never moves the ref.
    let next_mapping_bytes = next_mapping.serialize_bounded(max_mapping_bytes)?;

    // 6. Persist the crash-recoverable pending publication record BEFORE moving the ref.
    let abs_mapping_path = resolve_absolute_mapping_path(mapping_path)?;
    let pending = PendingPublication {
        format_version: MAPPING_FORMAT_VERSION,
        mapping_path: abs_mapping_path,
        ref_name: ref_name.to_string(),
        old_ref_sha: old_ref_sha.map(str::to_string),
        new_commit_sha: new_commit_sha.clone(),
        mapping: next_mapping,
    };
    let mut pending_bytes = serde_json::to_vec_pretty(&pending).map_err(|e| {
        GitError::InvalidMapping(format!("failed to serialize pending record: {e}"))
    })?;
    pending_bytes.push(b'\n');
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
    //    filters, then remove the pending journal.
    #[cfg(test)]
    test_hooks::during_worktree_sync()?;
    sync_worktree_to_commit_no_filters(
        clean_repo,
        git_branch,
        prev_head_sha.as_deref(),
        &new_commit_sha,
    )?;
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
    let max_pending = max_mapping_bytes.saturating_add(64 * 1024);
    let raw = read_bounded_regular_file(pending_path, &label, max_pending)?;
    reject_duplicate_json_keys(&raw)
        .map_err(|e| GitError::InvalidMapping(format!("malformed pending record JSON: {e}")))?;
    let pending: PendingPublication = serde_json::from_slice(&raw).map_err(|e| {
        GitError::InvalidMapping(format!("invalid pending publication record: {e}"))
    })?;
    if pending.format_version != MAPPING_FORMAT_VERSION {
        return Err(GitError::UnsupportedFormatVersion {
            file: label,
            found: pending.format_version,
            supported: MAPPING_FORMAT_VERSION,
        });
    }
    let abs_mapping = resolve_absolute_mapping_path(mapping_path)?;
    if pending.ref_name != ref_name
        || pending.mapping.dataset_id != dataset_id
        || pending.mapping.branch_id != branch_id
        || pending.mapping.git_branch != git_branch
        || pending.mapping.syntax != syntax
        || pending.mapping_path != abs_mapping
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
    if matches_old {
        // Previous export failed before advancing `ref_name`; clear the stale pending file.
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

    // `ref_name` already advanced to `pending.new_commit_sha`. Verify no manual edits were
    // made to tracked files or the index before finishing the checkpoint and worktree sync.
    ensure_clean_for_pending_recovery(
        clean_repo,
        git_dir,
        ref_name,
        pending.old_ref_sha.as_deref(),
        &pending.new_commit_sha,
    )?;

    // Check existing mapping consistency, then complete writing the pending mapping if not
    // yet persisted.
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

    if !already_persisted {
        pending
            .mapping
            .write_with_max_bytes(mapping_path, max_mapping_bytes)?;
    }

    // Determine which commit's tree is currently materialized in the working tree so stale
    // files deleted by `pending.new_commit_sha` are cleanly removed on recovery.
    let head_sym = run_git(clean_repo, &["symbolic-ref", "-q", "HEAD"], &[])
        .ok()
        .map(|s| s.trim().to_string());
    let prev_worktree_sha = if head_sym.as_deref() == Some(ref_name) {
        pending.old_ref_sha.clone()
    } else {
        git_rev_parse_opt(clean_repo, "HEAD")?
    };

    sync_worktree_to_commit_no_filters(
        clean_repo,
        git_branch,
        prev_worktree_sha.as_deref(),
        &pending.new_commit_sha,
    )?;
    remove_pending_file(pending_path)?;
    Ok(())
}

fn ensure_clean_for_pending_recovery(
    clean_repo: &Path,
    git_dir: &Path,
    ref_name: &str,
    old_ref_sha: Option<&str>,
    new_commit_sha: &str,
) -> GitResult<()> {
    ensure_no_in_progress_git_op(git_dir)?;

    let head_sym = run_git(clean_repo, &["symbolic-ref", "-q", "HEAD"], &[])
        .ok()
        .map(|s| s.trim().to_string());
    if head_sym.as_deref() != Some(ref_name) {
        // HEAD is on a different branch (or detached), so normal clean-state checks apply.
        return ensure_clean_git_state(clean_repo, git_dir);
    }

    // HEAD points to `ref_name`, which moved from `old_ref_sha` to `new_commit_sha`.
    // Working tree must have no unstaged modifications relative to the index, and the index
    // must match either `old_ref_sha` (pre-sync) or `new_commit_sha` (post-sync).
    let unstaged = run_git(clean_repo, &["diff", "--name-only"], &[])?;
    if !unstaged.trim().is_empty() {
        return Err(GitError::InvalidMapping(
            "refusing pending recovery with uncommitted modifications to tracked files".into(),
        ));
    }

    let staged_files = run_git(clean_repo, &["ls-files", "--cached"], &[])?;
    if staged_files.trim().is_empty() && old_ref_sha.is_none() {
        return Ok(());
    }
    let idx_tree = run_git(clean_repo, &["write-tree"], &[])?
        .trim()
        .to_string();
    let new_tree = run_git(
        clean_repo,
        &["rev-parse", &format!("{new_commit_sha}^{{tree}}")],
        &[],
    )?
    .trim()
    .to_string();
    if idx_tree.eq_ignore_ascii_case(&new_tree) {
        return Ok(());
    }
    if let Some(old_sha) = old_ref_sha {
        let old_tree = run_git(
            clean_repo,
            &["rev-parse", &format!("{old_sha}^{{tree}}")],
            &[],
        )?
        .trim()
        .to_string();
        if idx_tree.eq_ignore_ascii_case(&old_tree) {
            return Ok(());
        }
    }
    Err(GitError::InvalidMapping(
        "refusing pending recovery because the Git index does not match either the pre-commit \
         or post-commit tree"
            .into(),
    ))
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

fn resolve_absolute_mapping_path(mapping_path: &Path) -> GitResult<PathBuf> {
    let parent = effective_parent(mapping_path)?;
    let canon_parent = fs::canonicalize(&parent).map_err(|e| GitError::io(&parent, e))?;
    let name = mapping_path
        .file_name()
        .ok_or_else(|| GitError::UnsafePath {
            path: mapping_path.display().to_string(),
            reason: "mapping path must name a file".into(),
        })?;
    Ok(canon_parent.join(name))
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
/// any Git commits, including bare relative paths such as `mapping.json`.
fn prepare_mapping_path_safe(path: &Path) -> GitResult<()> {
    let parent = effective_parent(path)?;
    ensure_dir_no_symlink(&parent)?;
    check_dest_file_not_symlink(path)?;
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
    Ok(resolved)
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

fn ensure_clean_git_state(repo_dir: &Path, git_dir: &Path) -> GitResult<()> {
    ensure_no_in_progress_git_op(git_dir)?;
    let has_head = git_rev_parse_opt(repo_dir, "HEAD")?.is_some();
    if has_head {
        let staged = run_git(repo_dir, &["diff", "--cached", "--name-only"], &[])?;
        if !staged.trim().is_empty() {
            return Err(GitError::InvalidMapping(
                "refusing to export into a Git repository with pre-existing staged changes in the \
                 index"
                    .into(),
            ));
        }
        let unstaged = run_git(repo_dir, &["diff", "--name-only"], &[])?;
        if !unstaged.trim().is_empty() {
            return Err(GitError::InvalidMapping(
                "refusing to export into a Git repository with uncommitted modifications to \
                 tracked files"
                    .into(),
            ));
        }
    } else {
        let staged = run_git(repo_dir, &["ls-files", "--cached"], &[])?;
        if !staged.trim().is_empty() {
            return Err(GitError::InvalidMapping(
                "refusing to export into an unborn Git repository with pre-existing staged files \
                 in the index"
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
    let Some(sha) = commit_sha else {
        return Ok(HashSet::new());
    };
    let raw = run_git_bytes(
        clean_repo,
        &["ls-tree", "-r", "--name-only", "-z", sha],
        &[],
    )?;
    let mut set = HashSet::new();
    for entry in raw.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        set.insert(String::from_utf8_lossy(entry).into_owned());
    }
    Ok(set)
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
/// `git_branch` by writing exact raw blob bytes (`git cat-file blob`, bypassing all
/// `.gitattributes` / `.git/info/attributes` EOL and smudge filters) and resetting the
/// index via `git read-tree --reset`.
fn sync_worktree_to_commit_no_filters(
    clean_repo: &Path,
    git_branch: &str,
    previous_commit_sha: Option<&str>,
    commit_sha: &str,
) -> GitResult<()> {
    let old_tracked = list_tracked_paths_in_commit(clean_repo, previous_commit_sha)?;
    let raw_tree = run_git_bytes(clean_repo, &["ls-tree", "-r", "-z", commit_sha], &[])?;

    let mut target_entries: Vec<(String, String)> = Vec::new();
    let mut target_paths: HashSet<String> = HashSet::new();
    for rec in raw_tree.split(|&b| b == 0).filter(|s| !s.is_empty()) {
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
        let _mode = parts.next();
        let kind = parts.next().unwrap_or("");
        let oid = parts
            .next()
            .ok_or_else(|| GitError::InvalidMapping("missing OID in git ls-tree".into()))?;
        if kind != "blob" {
            continue;
        }
        validate_relative_path(rel)?;
        target_paths.insert(rel.to_string());
        target_entries.push((oid.to_string(), rel.to_string()));
    }

    let path_refs: Vec<&str> = target_entries.iter().map(|(_, p)| p.as_str()).collect();
    preflight_worktree_collisions(clean_repo, &old_tracked, &path_refs)?;

    // Write every tracked blob byte-exact without running Git checkout/smudge filters.
    for (oid, rel) in &target_entries {
        let blob_bytes = run_git_bytes(clean_repo, &["cat-file", "blob", oid], &[])?;
        let full = resolve_safe_path(clean_repo, rel)?;
        let parent = effective_parent(&full)?;
        ensure_dir_no_symlink(&parent)?;
        let needs_write = match fs::symlink_metadata(&full) {
            Ok(meta) if meta.file_type().is_file() => match fs::read(&full) {
                Ok(existing) => existing != blob_bytes,
                Err(_) => true,
            },
            _ => true,
        };
        if needs_write {
            write_atomic_safe(&full, &blob_bytes)?;
        }
    }

    // Remove previously tracked files that are absent from the target commit (leaving
    // unmanaged untracked files completely untouched).
    for old_rel in &old_tracked {
        if target_paths.contains(old_rel) {
            continue;
        }
        if validate_relative_path(old_rel).is_err() {
            continue;
        }
        let full = resolve_safe_path(clean_repo, old_rel)?;
        match fs::symlink_metadata(&full) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: old_rel.clone(),
                        reason: "tracked path in repository must not be a symlink".into(),
                    });
                }
                if meta.file_type().is_file() {
                    fs::remove_file(&full).map_err(|e| GitError::io(&full, e))?;
                    if let Some(p) = full.parent() {
                        let _ = crate::fsutil::sync_dir(p);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(&full, e)),
        }
    }

    let ref_name = format!("refs/heads/{git_branch}");
    run_git(clean_repo, &["symbolic-ref", "HEAD", &ref_name], &[])?;
    run_git(clean_repo, &["read-tree", "--reset", commit_sha], &[])?;
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        pub inject_concurrent_stage: Option<(String, Vec<u8>)>,
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

    pub(super) fn during_worktree_sync() -> GitResult<()> {
        FAULTS.with(|f| {
            let mut s = f.borrow_mut();
            if s.fail_worktree_sync {
                s.fail_worktree_sync = false;
                return Err(GitError::Io {
                    path: None,
                    source: io::Error::other("injected failure during worktree sync"),
                });
            }
            Ok(())
        })
    }

    pub(super) fn after_clean_check(repo_dir: &Path) {
        let injected = FAULTS.with(|f| f.borrow_mut().inject_concurrent_stage.take());
        if let Some((rel, bytes)) = injected {
            let p = repo_dir.join(&rel);
            fs::write(&p, bytes).unwrap();
            let status = Command::new("git")
                .args(["-C", repo_dir.to_str().unwrap(), "add", &rel])
                .status()
                .unwrap();
            assert!(status.success());
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
    fn isolated_index_ignores_file_staged_after_clean_check() {
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

        let repo = tempfile::tempdir().unwrap();
        {
            let _g = test_hooks::install(test_hooks::FaultState {
                inject_concurrent_stage: Some(("SNEAKY.txt".into(), b"do not commit\n".to_vec())),
                ..Default::default()
            });
            let report =
                export_branch_to_git(&ds, repo.path(), &GitExportOptions::default()).unwrap();
            assert_eq!(report.commits_exported, 1);
        }
        let tree_files =
            run_git(repo.path(), &["ls-tree", "-r", "--name-only", "HEAD"], &[]).unwrap();
        assert!(
            !tree_files.contains("SNEAKY.txt"),
            "concurrently staged file leaked into Sparkles commit: {tree_files}"
        );
        verify_git_tree(repo.path()).unwrap();
    }
}
