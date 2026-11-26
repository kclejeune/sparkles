//! Git export of a Sparkles dataset branch (F25 Phase 2).
//!
//! Exports a branch's history into a Git repository either commit-by-commit
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
    WriteOptions, ensure_child_dir_no_symlink, ensure_dir_no_symlink, read_bounded_regular_file,
    reject_duplicate_json_keys, sanitize_slug, sha256_hex, verify_existing_dir_no_symlink,
    write_atomic_safe, write_snapshot_git_tree_tracked,
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
/// Directory inside `.git/` where Sparkles export metadata and locks live.
pub const DEFAULT_SPARKLES_GIT_SUBDIR: &str = "sparkles";
/// Directory inside `.git/sparkles/` holding per-branch bidirectional mapping files.
pub const DEFAULT_MAPPINGS_SUBDIR: &str = "mappings";

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

    /// Read and validate a versioned mapping file, refusing unknown `format_version` with
    /// [`GitError::UnsupportedFormatVersion`], symlinks/non-regular files with
    /// [`GitError::UnsafePath`], and duplicate JSON keys with [`GitError::InvalidMapping`].
    pub fn read(path: &Path) -> GitResult<Self> {
        let label = path.display().to_string();
        let bytes = read_bounded_regular_file(path, &label, DEFAULT_MAX_MANIFEST_BYTES)?;
        parse_and_validate_mapping(&bytes, &label)
    }

    /// Persist this mapping file atomically with deterministic formatting via an
    /// exclusively created temp file, rejecting symlinked parents or targets.
    pub fn write(&self, path: &Path) -> GitResult<()> {
        if let Some(parent) = path.parent() {
            ensure_dir_no_symlink(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| GitError::InvalidMapping(format!("failed to serialize mapping: {e}")))?;
        bytes.push(b'\n');
        write_atomic_safe(path, &bytes)
    }
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
        if entry.mode == GitExportMode::Commits
            && !seen_commits.insert((entry.sparkles_branch_id, entry.sparkles_seq))
        {
            return Err(GitError::InvalidMapping(format!(
                "duplicate sparkles commit ({}, {}) in per-commit mapping",
                entry.sparkles_branch_id, entry.sparkles_seq
            )));
        }
    }

    Ok(mapping)
}

fn is_git_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Export a Sparkles dataset branch to a Git repository at `repo_dir`.
pub fn export_branch_to_git(
    dataset: &Dataset,
    repo_dir: &Path,
    options: &GitExportOptions,
) -> GitResult<GitExportReport> {
    validate_extra_trailers(&options.extra_trailers)?;

    // Resolve the owning store when invoked on a branch handle so inherited history,
    // merge/replay provenance, inherited snapshots, and retired ancestor authors match
    // calling `export_git` on the owning dataset with `options.branch`.
    let owner_state = if dataset.store().is_branch() {
        let own_branch = dataset.store().branch_name();
        if let Some(requested) = options.branch.as_deref()
            && requested != own_branch
        {
            return Err(GitError::Store(crate::Error::invalid(format!(
                "cannot export branch {requested:?} from a handle bound to branch {own_branch:?}"
            ))));
        }
        let state = dataset.owner_state().ok_or_else(|| {
            GitError::Store(crate::Error::invalid(
                "cannot export from a branch handle whose owning dataset is no longer open; \
                 call export_git on the owning dataset with GitExportOptions::branch",
            ))
        })?;
        Some((state, own_branch))
    } else {
        None
    };

    let (store, target_branch): (&Store, String) = match &owner_state {
        Some((state, own_branch)) => {
            // Re-resolve the branch's current name by UUID in case it was renamed since
            // the branch handle was opened.
            let live_name = state
                .store
                .listed_branch_name(
                    dataset.store().branch_id(),
                    &crate::guard::WriteOptions::default(),
                )?
                .unwrap_or_else(|| own_branch.clone());
            (&state.store, live_name)
        }
        None => {
            let b = options
                .branch
                .clone()
                .unwrap_or_else(|| dataset.store().branch_name());
            (dataset.store(), b)
        }
    };

    let git_branch = options
        .git_branch
        .clone()
        .unwrap_or_else(|| target_branch.clone());
    validate_git_branch_name(&git_branch)?;

    // Gather target branch identity, prefixes, and validated commit range in ascending order.
    let BranchHistory {
        dataset_id,
        branch_id,
        prefixes: branch_prefixes,
        mut commits,
    } = collect_branch_history(store, &target_branch, options)?;

    if commits.is_empty() {
        return Err(GitError::HistoryUnavailable(format!(
            "no commits matched export range on branch {target_branch:?}"
        )));
    }

    // Ensure `repo_dir` is a safe standalone Git repository root (never hijacking an
    // enclosing ancestor repo or following symlinks).
    let git_dir = ensure_standalone_git_repo(repo_dir, &git_branch)?;

    // Serialize concurrent exports to the same repository.
    let _repo_lock = acquire_export_lock(&git_dir)?;

    // Refuse if the Git index has pre-existing staged changes or dirty tracked files so we
    // never sweep unrelated staged changes into Sparkles commits or clobber local work.
    ensure_clean_git_state(repo_dir)?;

    let mapping_path = match &options.mapping_path {
        Some(p) => {
            ensure_mapping_path_safe(p)?;
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

    // Inspect target Git branch tip and existing mapping BEFORE switching branches or
    // mutating the working tree.
    let ref_name = format!("refs/heads/{git_branch}");
    let branch_head = git_rev_parse_opt(repo_dir, &ref_name)?;
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
        repo_dir,
        &ref_name,
        branch_head.as_deref(),
        &mapping_path,
        mapping_exists,
        dataset_id,
        branch_id,
        &git_branch,
        options,
        &commits,
    )?;

    // Populate change-log metadata (author, message) ONLY for commits that will actually be
    // exported, keeping metadata I/O proportional to newly exported commits.
    match options.mode {
        GitExportMode::Commits => {
            if skipped < commits.len() {
                populate_changelog_metadata(store, &mut commits[skipped..]);
            }
        }
        GitExportMode::Squash => {
            if !squash_already_up_to_date {
                let last_idx = commits.len() - 1;
                populate_changelog_metadata(store, &mut commits[last_idx..]);
            }
        }
    }

    // Check out the target Git branch safely (using a clean orphan branch if it does not
    // exist yet in an existing non-empty repository).
    checkout_target_git_branch(repo_dir, &git_branch, branch_head.is_some())?;

    let write_opts = WriteOptions {
        syntax: options.syntax,
        extra_prefixes: options.extra_prefixes.clone(),
        prune_stale_graphs: true,
        ..WriteOptions::default()
    };

    let mut exported_count = 0usize;

    match options.mode {
        GitExportMode::Commits => {
            for item in commits.iter().skip(skipped) {
                let snap = snapshot_for_commit(store, &target_branch, item.commit.seq)?;
                let (manifest, pruned_paths) = write_snapshot_git_tree_tracked(
                    snap.as_ref(),
                    &branch_prefixes,
                    repo_dir,
                    &write_opts,
                )?;

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
                let git_sha = git_stage_and_commit_exact(
                    repo_dir,
                    &manifest,
                    &pruned_paths,
                    &message,
                    &author_name,
                    &author_email,
                    item.commit.timestamp_ms,
                )?;
                entries.push(CommitMappingEntry {
                    sparkles_branch_id: item.branch_id,
                    sparkles_branch: item.branch.clone(),
                    sparkles_seq: item.commit.seq,
                    git_sha,
                    mode: GitExportMode::Commits,
                    timestamp_ms: item.commit.timestamp_ms,
                    squashed_from: None,
                });
                exported_count += 1;

                // Persist the updated mapping after every commit so partial failures remain
                // cleanly resumable without duplicating already-published Git commits.
                let checkpoint = CommitMapping {
                    format_version: MAPPING_FORMAT_VERSION,
                    profile: MAPPING_PROFILE_NAME.to_string(),
                    dataset_id,
                    branch: target_branch.clone(),
                    branch_id,
                    git_branch: git_branch.clone(),
                    syntax: options.syntax,
                    entries: entries.clone(),
                };
                checkpoint.write(&mapping_path)?;
            }
        }
        GitExportMode::Squash => {
            if !squash_already_up_to_date {
                let head_item = commits.last().expect("commits non-empty");
                let first_seq = commits.first().expect("commits non-empty").commit.seq;
                let squashed_from = (commits.len() > 1).then_some(first_seq);

                let snap = snapshot_for_commit(store, &target_branch, head_item.commit.seq)?;
                let (manifest, pruned_paths) = write_snapshot_git_tree_tracked(
                    snap.as_ref(),
                    &branch_prefixes,
                    repo_dir,
                    &write_opts,
                )?;

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
                let git_sha = git_stage_and_commit_exact(
                    repo_dir,
                    &manifest,
                    &pruned_paths,
                    &message,
                    &author_name,
                    &author_email,
                    head_item.commit.timestamp_ms,
                )?;
                entries.push(CommitMappingEntry {
                    sparkles_branch_id: head_item.branch_id,
                    sparkles_branch: head_item.branch.clone(),
                    sparkles_seq: head_item.commit.seq,
                    git_sha,
                    mode: GitExportMode::Squash,
                    timestamp_ms: head_item.commit.timestamp_ms,
                    squashed_from,
                });
                exported_count = 1;
                skipped = 0;
            }
        }
    }

    let head_git_sha = entries
        .last()
        .map(|e| e.git_sha.clone())
        .ok_or_else(|| GitError::HistoryUnavailable("no commits were exported".into()))?;

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
    mapping.write(&mapping_path)?;

    Ok(GitExportReport {
        repo_dir: repo_dir.to_path_buf(),
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
    commits: &[PlannedCommit],
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

    let existing = CommitMapping::read(mapping_path)?;
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

    // Verify every mapped SHA is present in exact first-parent chronological order on the ref.
    let lineage_raw = run_git(
        repo_dir,
        &["rev-list", "--first-parent", "--reverse", ref_name],
        &[],
    )?;
    let lineage: Vec<&str> = lineage_raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if lineage.len() != existing.entries.len()
        || !lineage
            .iter()
            .zip(existing.entries.iter())
            .all(|(actual, entry)| actual.eq_ignore_ascii_case(&entry.git_sha))
    {
        return Err(GitError::InvalidMapping(format!(
            "git branch {git_branch:?} first-parent history does not match mapping {}",
            mapping_path.display()
        )));
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
            if last.sparkles_branch_id == head_item.branch_id
                && last.sparkles_seq == head_item.commit.seq
                && last.squashed_from == planned_squashed_from
            {
                return Ok((existing.entries, 1, true));
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
}

fn collect_branch_history(
    store: &Store,
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

    let dataset_id = store.owner_dataset_id();
    let (branch_id, head_seq, prefixes, raw_commits) =
        match store.branch_commits(target_branch, CommitRange::Latest, usize::MAX) {
            Ok(mut list) => {
                list.reverse();
                let bid = store.branch_id_of(target_branch)?;
                let target_store = store.branch(target_branch)?;
                let head_seq = target_store.head_commit().seq;
                let prefixes = target_store.prefixes();
                (bid, head_seq, prefixes, list)
            }
            Err(e) if target_branch == MAIN => {
                // Fallback for stores where branch operations are unsupported.
                let mut page = store.commits(CommitRange::Latest, usize::MAX).commits;
                page.reverse();
                let bid = store.branch_id();
                let head_seq = store.head_commit().seq;
                let list: Vec<BranchCommit> = page
                    .into_iter()
                    .map(|c| BranchCommit {
                        annotation: store.annotation(c.seq),
                        commit: c,
                        branch: Some(MAIN.to_string()),
                        branch_id: bid,
                        merged_from: None,
                        replayed_from: None,
                    })
                    .collect();
                let _ = e;
                (bid, head_seq, store.prefixes(), list)
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
    // explicitly requested via `include_empty_root` or `from_seq == Some(0)`.
    if !options.include_empty_root
        && options.from_seq != Some(0)
        && planned.len() > 1
        && planned[0].commit.seq == 0
        && planned[0].commit.kind == CommitKind::Create
        && planned[0].commit.quads == 0
        && planned[0].message.is_none()
    {
        planned.remove(0);
    }

    Ok(BranchHistory {
        dataset_id,
        branch_id,
        prefixes,
        commits: planned,
    })
}

/// Populate `(author, message)` from each owning branch store's change log for ONLY the
/// commits about to be exported. Flushes pending change-log records automatically and
/// resolves retired ancestor branches by UUID.
fn populate_changelog_metadata(store: &Store, commits: &mut [PlannedCommit]) {
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
        let res = if bid == store.branch_id() {
            store.commit_change_metas(&seqs)
        } else {
            store.branch_commit_change_metas(bid, &seqs)
        };
        if let Ok(map) = res {
            metas_by_branch.insert(bid, map);
        }
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
}

fn snapshot_for_commit(store: &Store, target_branch: &str, seq: u64) -> GitResult<Arc<Snapshot>> {
    let at = At::Commit(seq);
    let opts = HistoryOptions::default();
    let res = match store.branch_snapshot_at(target_branch, &at, &opts) {
        Ok(v) => Ok(v),
        Err(_) if target_branch == MAIN => store.snapshot_at(&at, &opts),
        Err(e) => Err(e),
    };
    match res {
        Ok((snap, _)) => Ok(snap),
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

/// Validate a Git branch name against `git-check-ref-format(1)` rules.
fn validate_git_branch_name(name: &str) -> GitResult<()> {
    let invalid = || GitError::UnsafePath {
        path: name.to_string(),
        reason: "invalid git branch name".into(),
    };
    if name.is_empty()
        || name == "@"
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

fn ensure_mapping_path_safe(path: &Path) -> GitResult<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(GitError::UnsafePath {
                        path: parent.display().to_string(),
                        reason: "mapping parent directory must not be a symlink".into(),
                    });
                }
                if !meta.file_type().is_dir() {
                    return Err(GitError::UnsafePath {
                        path: parent.display().to_string(),
                        reason: "mapping parent path is not a directory".into(),
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(GitError::io(parent, e)),
        }
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: path.display().to_string(),
                    reason: "mapping path must not be a symlink".into(),
                });
            }
            if !meta.file_type().is_file() {
                return Err(GitError::UnsafePath {
                    path: path.display().to_string(),
                    reason: "mapping path must be a regular file".into(),
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(GitError::io(path, e)),
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

/// Ensure `repo_dir` is a standalone non-bare Git repository root (never an ancestor
/// repository or symlink) and return its resolved `.git` directory.
fn ensure_standalone_git_repo(repo_dir: &Path, git_branch: &str) -> GitResult<PathBuf> {
    ensure_dir_no_symlink(repo_dir)?;
    let dot_git = repo_dir.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GitError::UnsafePath {
                    path: dot_git.display().to_string(),
                    reason: "repository .git entry must not be a symlink".into(),
                });
            }
            resolve_exact_git_dir(repo_dir)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Initialize a brand-new standalone repository directly inside `repo_dir`,
            // even if `repo_dir` is nested inside an enclosing Git working tree.
            run_git(repo_dir, &["init", "-q", "-b", git_branch], &[])?;
            resolve_exact_git_dir(repo_dir)
        }
        Err(e) => Err(GitError::io(&dot_git, e)),
    }
}

/// Verify that `repo_dir` itself is the exact top-level working tree of a non-bare Git
/// repository and return its resolved `.git` directory.
fn resolve_exact_git_dir(repo_dir: &Path) -> GitResult<PathBuf> {
    verify_existing_dir_no_symlink(repo_dir, &repo_dir.display().to_string())?;
    let dot_git = repo_dir.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(GitError::UnsafePath {
                path: dot_git.display().to_string(),
                reason: "repository .git entry must not be a symlink".into(),
            });
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(GitError::InvalidMapping(format!(
                "{} is not a Git repository root (no .git entry)",
                repo_dir.display()
            )));
        }
        Err(e) => return Err(GitError::io(&dot_git, e)),
    }

    let inside = run_git(repo_dir, &["rev-parse", "--is-inside-work-tree"], &[])?;
    if inside.trim() != "true" {
        return Err(GitError::InvalidMapping(format!(
            "{} is not a non-bare Git working tree",
            repo_dir.display()
        )));
    }
    let toplevel = run_git(repo_dir, &["rev-parse", "--show-toplevel"], &[])?;
    let canon_top = fs::canonicalize(toplevel.trim()).map_err(|e| GitError::io(repo_dir, e))?;
    let canon_repo = fs::canonicalize(repo_dir).map_err(|e| GitError::io(repo_dir, e))?;
    if canon_top != canon_repo {
        return Err(GitError::InvalidMapping(format!(
            "{} is not the Git repository root (top-level is {})",
            repo_dir.display(),
            canon_top.display()
        )));
    }

    let git_dir_raw = run_git(repo_dir, &["rev-parse", "--git-dir"], &[])?;
    let git_dir = PathBuf::from(git_dir_raw.trim());
    let resolved = if git_dir.is_absolute() {
        git_dir
    } else {
        repo_dir.join(git_dir)
    };
    verify_existing_dir_no_symlink(&resolved, &resolved.display().to_string())?;
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

fn ensure_clean_git_state(repo_dir: &Path) -> GitResult<()> {
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

fn checkout_target_git_branch(
    repo_dir: &Path,
    git_branch: &str,
    branch_exists: bool,
) -> GitResult<()> {
    let current = run_git(repo_dir, &["symbolic-ref", "--short", "HEAD"], &[])
        .ok()
        .map(|s| s.trim().to_string());
    if branch_exists {
        if current.as_deref() != Some(git_branch) {
            run_git(repo_dir, &["checkout", "-q", git_branch], &[])?;
        }
        return Ok(());
    }

    let ref_name = format!("refs/heads/{git_branch}");
    if git_rev_parse_opt(repo_dir, "HEAD")?.is_some() {
        // Create a clean orphan branch so a new exported branch never inherits an
        // unrelated checked-out branch's commits or tracked files.
        run_git(repo_dir, &["checkout", "-q", "--orphan", git_branch], &[])?;
        run_git(repo_dir, &["rm", "-rf", "-q", "--ignore-unmatch", "."], &[])?;
    } else if current.as_deref() != Some(git_branch) {
        run_git(repo_dir, &["symbolic-ref", "HEAD", &ref_name], &[])?;
    }
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

/// Stage ONLY the exact canonical profile files (bypassing `.gitattributes` filters via
/// `hash-object -w --no-filters` + `update-index --index-info`) and prior profile-owned
/// deletions, then commit with `--no-verify --cleanup=verbatim -F -`.
fn git_stage_and_commit_exact(
    repo_dir: &Path,
    manifest: &crate::git::CanonicalManifest,
    pruned_paths: &[String],
    message: &str,
    author_name: &str,
    author_email: &str,
    timestamp_ms: i64,
) -> GitResult<String> {
    let mut index_lines = String::new();
    let mut stage_paths: Vec<&str> = Vec::with_capacity(1 + manifest.graphs.len());
    stage_paths.push(MANIFEST_REL_PATH);
    for entry in &manifest.graphs {
        stage_paths.push(entry.path.as_str());
    }

    for rel in stage_paths {
        let oid = run_git(
            repo_dir,
            &["hash-object", "-w", "--no-filters", "--", rel],
            &[],
        )?;
        let oid = oid.trim();
        index_lines.push_str("100644 ");
        index_lines.push_str(oid);
        index_lines.push('\t');
        index_lines.push_str(rel);
        index_lines.push('\n');
    }

    run_git_with_stdin(
        repo_dir,
        &["update-index", "--add", "--index-info"],
        &[],
        index_lines.as_bytes(),
    )?;

    if !pruned_paths.is_empty() {
        let mut args: Vec<&str> = Vec::with_capacity(2 + pruned_paths.len());
        args.push("update-index");
        args.push("--force-remove");
        args.push("--");
        for p in pruned_paths {
            args.push(p.as_str());
        }
        run_git(repo_dir, &args, &[])?;
    }

    // Clamp negative pre-1970 timestamps to 0 for Git's unsigned epoch-seconds parser;
    // the exact RFC 3339 timestamp is preserved in the `Sparkles-Timestamp` trailer.
    let secs = timestamp_ms.div_euclid(1000).max(0);
    let git_date = format!("{secs} +0000");
    let envs = [
        ("GIT_AUTHOR_NAME", author_name),
        ("GIT_AUTHOR_EMAIL", author_email),
        ("GIT_AUTHOR_DATE", git_date.as_str()),
        ("GIT_COMMITTER_NAME", author_name),
        ("GIT_COMMITTER_EMAIL", author_email),
        ("GIT_COMMITTER_DATE", git_date.as_str()),
    ];
    run_git_with_stdin(
        repo_dir,
        &[
            "commit",
            "-q",
            "--no-verify",
            "--cleanup=verbatim",
            "--allow-empty",
            "-F",
            "-",
        ],
        &envs,
        message.as_bytes(),
    )?;
    // Refresh index stat cache silently when possible.
    let _ = git_command(repo_dir)
        .args(["update-index", "-q", "--refresh"])
        .output();

    let sha = run_git(repo_dir, &["rev-parse", "HEAD"], &[])?;
    Ok(sha.trim().to_string())
}

fn run_git(repo_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitResult<String> {
    let mut cmd = git_command(repo_dir);
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_git_with_stdin(
    repo_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin_bytes: &[u8],
) -> GitResult<String> {
    let mut cmd = git_command(repo_dir);
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
mod tests {
    use super::*;

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
        for good in ["main", "feature/vocab-v1", "release/1.2.3", "a_b.c"] {
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
}
