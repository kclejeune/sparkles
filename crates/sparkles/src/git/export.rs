//! Git export of a Sparkles dataset branch (F25 Phase 2).
//!
//! Exports a branch's history into a Git repository either commit-by-commit
//! ([`GitExportMode::PerCommit`]) or as a single squashed commit
//! ([`GitExportMode::Squash`]). Each Git commit carries the canonical tree produced by
//! [`crate::git::profile`], deterministic author/committer identity and UTC timestamps
//! derived from the Sparkles commits, RFC 822 commit trailers, and a versioned
//! bidirectional Sparkles-commit to Git-SHA mapping record (`format_version = 1`).

use crate::Dataset;
use crate::branch::{CommitRef, MAIN, NamedCommitRef};
use crate::commit::{CommitInfo, CommitKind, CommitRange, rfc3339_ms};
use crate::fsutil;
use crate::git::profile::{
    CanonicalSyntax, MANIFEST_FORMAT_VERSION, WriteOptions, write_snapshot_git_tree,
};
use crate::git::{GitError, GitResult};
use crate::history::{At, HistoryOptions};
use crate::store::{BranchCommit, HistoryBound, HistoryQuery, Snapshot, Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

/// Supported format version of the bidirectional commit mapping record.
pub const MAPPING_FORMAT_VERSION: u32 = 1;
/// Stable profile identifier recorded in the mapping file.
pub const MAPPING_PROFILE_NAME: &str = "sparkles-git-mapping-v1";
/// Relative path inside `.git/` where the mapping record is stored by default.
pub const DEFAULT_MAPPING_GIT_SUBPATH: &str = "sparkles/mapping.json";

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
    pub extra_trailers: Vec<(String, String)>,
    /// Override location for the versioned bidirectional mapping file. When `None`, stored
    /// at `<git-dir>/sparkles/mapping.json`.
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

/// Versioned bidirectional mapping between Sparkles commits and Git commit SHAs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitMapping {
    pub format_version: u32,
    pub profile: String,
    pub dataset_id: Uuid,
    pub branch: String,
    pub branch_id: Uuid,
    pub git_branch: String,
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
    /// [`GitError::UnsupportedFormatVersion`].
    pub fn read(path: &Path) -> GitResult<Self> {
        let bytes = fs::read(path).map_err(|e| GitError::io(path, e))?;
        parse_and_validate_mapping(&bytes, &path.display().to_string())
    }

    /// Persist this mapping file atomically with deterministic formatting.
    pub fn write(&self, path: &Path) -> GitResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| GitError::io(parent, e))?;
        }
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| GitError::InvalidMapping(format!("failed to serialize mapping: {e}")))?;
        bytes.push(b'\n');
        fsutil::write_atomic(path, &bytes).map_err(|e| GitError::io(path, e))
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

/// Return the default mapping path (`<git-dir>/sparkles/mapping.json`) for a Git repository.
pub fn default_mapping_path(repo_dir: &Path) -> GitResult<PathBuf> {
    let git_dir_raw = run_git(repo_dir, &["rev-parse", "--git-dir"], &[])?;
    let git_dir = PathBuf::from(git_dir_raw.trim());
    let resolved = if git_dir.is_absolute() {
        git_dir
    } else {
        repo_dir.join(git_dir)
    };
    Ok(resolved.join(DEFAULT_MAPPING_GIT_SUBPATH))
}

/// Parse and strictly validate a mapping JSON payload.
pub(crate) fn parse_and_validate_mapping(
    bytes: &[u8],
    file_label: &str,
) -> GitResult<CommitMapping> {
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

    let store = dataset.store();
    let target_branch = options
        .branch
        .clone()
        .unwrap_or_else(|| store.branch_name());
    let git_branch = options
        .git_branch
        .clone()
        .unwrap_or_else(|| target_branch.clone());
    validate_git_branch_name(&git_branch)?;

    // Gather target branch identity, prefixes, and full commit chain in ascending order.
    let BranchHistory {
        dataset_id,
        branch_id,
        prefixes: branch_prefixes,
        commits,
    } = collect_branch_history(store, &target_branch, options)?;

    if commits.is_empty() {
        return Err(GitError::HistoryUnavailable(format!(
            "no commits matched export range on branch {target_branch:?}"
        )));
    }

    // Initialize or open the Git repository and check out the target branch.
    ensure_git_repo(repo_dir, &git_branch)?;
    let mapping_path = match &options.mapping_path {
        Some(p) => p.clone(),
        None => default_mapping_path(repo_dir)?,
    };

    // Load existing mapping if present and matching this dataset/branch/git_branch.
    let mut existing_entries: Vec<CommitMappingEntry> = Vec::new();
    let mut skipped = 0usize;
    if mapping_path.is_file() {
        let existing = CommitMapping::read(&mapping_path)?;
        if existing.dataset_id == dataset_id
            && existing.branch_id == branch_id
            && existing.git_branch == git_branch
            && options.mode == GitExportMode::Commits
        {
            let head_sha = git_rev_parse_opt(repo_dir, "HEAD")?;
            if let Some(head_sha) = head_sha
                && let Some(last) = existing.entries.last()
                && last.git_sha.eq_ignore_ascii_case(&head_sha)
            {
                let mut prefix_len = 0usize;
                for (existing_entry, planned) in existing.entries.iter().zip(commits.iter()) {
                    if existing_entry.sparkles_branch_id == planned.branch_id
                        && existing_entry.sparkles_seq == planned.commit.seq
                        && existing_entry.mode == GitExportMode::Commits
                    {
                        prefix_len += 1;
                    } else {
                        break;
                    }
                }
                if prefix_len == existing.entries.len() {
                    existing_entries = existing.entries;
                    skipped = prefix_len;
                }
            }
        }
    }

    let write_opts = WriteOptions {
        syntax: options.syntax,
        extra_prefixes: options.extra_prefixes.clone(),
        prune_stale_graphs: true,
        ..WriteOptions::default()
    };

    let mut exported_count = 0usize;
    let mut entries = existing_entries;

    match options.mode {
        GitExportMode::Commits => {
            for item in commits.iter().skip(skipped) {
                let snap = snapshot_for_commit(store, &target_branch, item.commit.seq)?;
                write_snapshot_git_tree(snap.as_ref(), &branch_prefixes, repo_dir, &write_opts)?;

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
                let git_sha = git_stage_and_commit(
                    repo_dir,
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
            }
        }
        GitExportMode::Squash => {
            let head_item = commits.last().expect("commits non-empty");
            let first_seq = commits.first().expect("commits non-empty").commit.seq;
            let squashed_from = (commits.len() > 1).then_some(first_seq);

            let snap = snapshot_for_commit(store, &target_branch, head_item.commit.seq)?;
            write_snapshot_git_tree(snap.as_ref(), &branch_prefixes, repo_dir, &write_opts)?;

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
            let git_sha = git_stage_and_commit(
                repo_dir,
                &message,
                &author_name,
                &author_email,
                head_item.commit.timestamp_ms,
            )?;
            entries.clear();
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

struct ChangeLogMeta {
    author: Option<String>,
    message: Option<String>,
}

type StoreCommitMeta = HashMap<u64, ChangeLogMeta>;

fn collect_branch_history(
    store: &Store,
    target_branch: &str,
    options: &GitExportOptions,
) -> GitResult<BranchHistory> {
    let dataset_id = store.owner_dataset_id();
    let (branch_id, prefixes, raw_commits, authors_by_branch) = if store.is_branch() {
        if target_branch != store.branch_name() {
            return Err(GitError::Store(crate::Error::invalid(format!(
                "cannot export branch {target_branch:?} from a handle bound to branch {:?}",
                store.branch_name()
            ))));
        }
        let mut page = store.commits(CommitRange::Latest, usize::MAX).commits;
        page.reverse();
        let bid = store.branch_id();
        let bname = Some(store.branch_name());
        let authors = collect_authors_for_store(store);
        let mut by_branch: HashMap<Uuid, StoreCommitMeta> = HashMap::new();
        by_branch.insert(bid, authors);
        let list: Vec<BranchCommit> = page
            .into_iter()
            .map(|c| BranchCommit {
                annotation: store.annotation(c.seq),
                commit: c,
                branch: bname.clone(),
                branch_id: bid,
                merged_from: None,
                replayed_from: None,
            })
            .collect();
        (bid, store.prefixes(), list, by_branch)
    } else {
        match store.branch_commits(target_branch, CommitRange::Latest, usize::MAX) {
            Ok(mut list) => {
                list.reverse();
                let bid = store.branch_id_of(target_branch)?;
                let prefixes = store.branch(target_branch)?.prefixes();
                let mut authors_by_branch: HashMap<Uuid, StoreCommitMeta> = HashMap::new();
                // Collect authors per owning branch store that appears in the chain.
                let mut branch_names: HashMap<Uuid, Option<String>> = HashMap::new();
                for bc in &list {
                    branch_names
                        .entry(bc.branch_id)
                        .or_insert_with(|| bc.branch.clone());
                }
                for (b_uuid, maybe_name) in branch_names {
                    let map = if b_uuid == store.branch_id() {
                        collect_authors_for_store(store)
                    } else if let Some(name) = maybe_name.as_deref() {
                        match store.branch(name) {
                            Ok(bs) => collect_authors_for_store(&bs),
                            Err(_) => HashMap::new(),
                        }
                    } else {
                        HashMap::new()
                    };
                    authors_by_branch.insert(b_uuid, map);
                }
                (bid, prefixes, list, authors_by_branch)
            }
            Err(e) if target_branch == MAIN => {
                // Fallback for stores where branch operations are unsupported.
                let mut page = store.commits(CommitRange::Latest, usize::MAX).commits;
                page.reverse();
                let bid = store.branch_id();
                let authors = collect_authors_for_store(store);
                let mut by_branch: HashMap<Uuid, StoreCommitMeta> = HashMap::new();
                by_branch.insert(bid, authors);
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
                (bid, store.prefixes(), list, by_branch)
            }
            Err(e) => return Err(GitError::Store(e)),
        }
    };

    let mut planned: Vec<PlannedCommit> = raw_commits
        .into_iter()
        .map(|bc| {
            let changelog_meta = authors_by_branch
                .get(&bc.branch_id)
                .and_then(|m| m.get(&bc.commit.seq));
            let author = changelog_meta.and_then(|m| m.author.clone());
            let ann_message = bc
                .annotation
                .as_ref()
                .and_then(|a| a.message.as_ref().map(|m| m.to_string()));
            let message = ann_message.or_else(|| changelog_meta.and_then(|m| m.message.clone()));
            let digest_hex = bc.annotation.as_ref().and_then(|a| a.digest_hex());
            PlannedCommit {
                commit: bc.commit,
                branch: bc.branch,
                branch_id: bc.branch_id,
                merged_from: bc.merged_from,
                replayed_from: bc.replayed_from,
                message,
                digest_hex,
                author,
            }
        })
        .collect();

    // Apply optional from_seq / to_seq bounds.
    if let Some(from) = options.from_seq {
        planned.retain(|p| p.commit.seq >= from);
    }
    if let Some(to) = options.to_seq {
        planned.retain(|p| p.commit.seq <= to);
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

/// Read recorded `(author, message)` metadata per commit seq from a store's change log,
/// when enabled.
fn collect_authors_for_store(store: &Store) -> StoreCommitMeta {
    let mut map = HashMap::new();
    let head = store.head_commit().seq;
    if head == 0 {
        return map;
    }
    // Probe each commit with limit=1 so we read at most one quad per commit even on large
    // commits, while still recovering every commit's recorded author and message.
    for seq in 1..=head {
        let q = HistoryQuery {
            from: Some(HistoryBound::Commit(seq)),
            to: Some(HistoryBound::Commit(seq)),
            limit: 1,
            ..HistoryQuery::default()
        };
        match store.history_changes(&q) {
            Ok(res) => {
                if let Some(first) = res.changes.first() {
                    let author = first.commit.author.as_ref().map(|s| s.to_string());
                    let message = first.commit.message.as_ref().map(|s| s.to_string());
                    if author.is_some() || message.is_some() {
                        map.insert(seq, ChangeLogMeta { author, message });
                    }
                }
            }
            Err(_) => break,
        }
    }
    map
}

fn snapshot_for_commit(store: &Store, target_branch: &str, seq: u64) -> GitResult<Arc<Snapshot>> {
    let at = At::Commit(seq);
    let opts = HistoryOptions::default();
    let res = if store.is_branch() {
        store.snapshot_at(&at, &opts)
    } else {
        match store.branch_snapshot_at(target_branch, &at, &opts) {
            Ok(v) => Ok(v),
            Err(_) if target_branch == MAIN => store.snapshot_at(&at, &opts),
            Err(e) => Err(e),
        }
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
    let subject = match mode {
        GitExportMode::Squash => squash_message
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| item.message.clone())
            .unwrap_or_else(|| {
                format!(
                    "Sparkles export of {} at commit {}",
                    target_branch, item.commit.seq
                )
            }),
        GitExportMode::Commits => item.message.clone().unwrap_or_else(|| {
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
        let clean = raw_author.replace(['\r', '\n'], " ");
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

fn resolve_author(raw: Option<&str>, default_name: &str, default_email: &str) -> (String, String) {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return (default_name.to_string(), default_email.to_string());
    };
    let clean = raw.replace(['\r', '\n'], " ");
    // `Display Name <email@example.com>`
    if let (Some(lt), Some(gt)) = (clean.find('<'), clean.rfind('>'))
        && lt < gt
    {
        let name = clean[..lt].trim();
        let email = clean[lt + 1..gt].trim();
        if !email.is_empty() {
            let final_name = if name.is_empty() { email } else { name };
            return (final_name.to_string(), email.to_string());
        }
    }
    // Bare email address
    if clean.contains('@') && !clean.contains(' ') {
        return (clean.clone(), clean);
    }
    // Plain principal/username
    let local: String = clean
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let email = if local.is_empty() {
        default_email.to_string()
    } else {
        format!("{local}@sparkles.local")
    };
    (clean, email)
}

fn validate_extra_trailers(trailers: &[(String, String)]) -> GitResult<()> {
    for (k, v) in trailers {
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(GitError::InvalidMapping(format!(
                "invalid trailer key {k:?}: expected ASCII letters, digits, and '-'"
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

fn validate_git_branch_name(name: &str) -> GitResult<()> {
    if name.is_empty()
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err(GitError::UnsafePath {
            path: name.to_string(),
            reason: "invalid git branch name".into(),
        });
    }
    Ok(())
}

// ------------------------------------------------------------ git subprocesses -----

fn ensure_git_repo(repo_dir: &Path, git_branch: &str) -> GitResult<()> {
    fs::create_dir_all(repo_dir).map_err(|e| GitError::io(repo_dir, e))?;
    let is_repo = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|e| GitError::GitCommand {
            args: vec!["rev-parse".into(), "--is-inside-work-tree".into()],
            status: None,
            stderr: e.to_string(),
        })?;
    if !is_repo.status.success() {
        run_git(repo_dir, &["init", "-q", "-b", git_branch], &[])?;
        return Ok(());
    }

    // Check if the requested branch already exists or is the current unborn/checked-out branch.
    let current = run_git(repo_dir, &["symbolic-ref", "--short", "HEAD"], &[])
        .ok()
        .map(|s| s.trim().to_string());
    if current.as_deref() == Some(git_branch) {
        return Ok(());
    }
    let ref_name = format!("refs/heads/{git_branch}");
    let exists = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["show-ref", "--verify", "--quiet", &ref_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if exists {
        run_git(repo_dir, &["checkout", "-q", git_branch], &[])?;
    } else if git_rev_parse_opt(repo_dir, "HEAD")?.is_some() {
        run_git(repo_dir, &["checkout", "-q", "-b", git_branch], &[])?;
    } else {
        run_git(repo_dir, &["symbolic-ref", "HEAD", &ref_name], &[])?;
    }
    Ok(())
}

fn git_rev_parse_opt(repo_dir: &Path, rev: &str) -> GitResult<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
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

fn git_stage_and_commit(
    repo_dir: &Path,
    message: &str,
    author_name: &str,
    author_email: &str,
    timestamp_ms: i64,
) -> GitResult<String> {
    // Stage the canonical metadata and graph directories (including deletions).
    run_git(
        repo_dir,
        &["add", "-A", "--", ".sparkles-git", "graphs"],
        &[],
    )?;

    let secs = timestamp_ms.div_euclid(1000);
    let git_date = format!("{secs} +0000");
    let envs = [
        ("GIT_AUTHOR_NAME", author_name),
        ("GIT_AUTHOR_EMAIL", author_email),
        ("GIT_AUTHOR_DATE", git_date.as_str()),
        ("GIT_COMMITTER_NAME", author_name),
        ("GIT_COMMITTER_EMAIL", author_email),
        ("GIT_COMMITTER_DATE", git_date.as_str()),
    ];
    run_git(
        repo_dir,
        &["commit", "-q", "--allow-empty", "-m", message],
        &envs,
    )?;
    let sha = run_git(repo_dir, &["rev-parse", "HEAD"], &[])?;
    Ok(sha.trim().to_string())
}

fn run_git(repo_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitResult<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.quotepath=false",
        ])
        .args(args);
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
    fn author_resolution_handles_formats() {
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
    }

    #[test]
    fn mapping_refuses_unknown_format_version() {
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
    }
}
