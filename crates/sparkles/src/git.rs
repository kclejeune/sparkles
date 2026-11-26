//! Canonical git serialization profile and git branch export
//! ([F25](../../../docs/specs/F25-git-sync-and-remotes/SPEC.md), roadmap item 460).
//!
//! Phase 1 ([`profile`]) defines a versioned (`format_version = 1`) canonical directory
//! layout with one file per graph in deterministic sorted, subject-grouped Turtle or
//! sorted N-Quads, stable prefixes, and dataset-wide RDFC-1.0 canonical blank-node
//! labels, together with a strict reader and dataset importer.
//!
//! Phase 2 ([`export`]) exports a Sparkles dataset branch to a Git repository either
//! commit-by-commit or squashed, preserving author, timestamp, message, and RFC 822
//! commit trailers while recording a versioned bidirectional Sparkles-commit to Git-SHA
//! mapping.

pub mod export;
pub mod profile;

pub use export::{
    CommitMapping, CommitMappingEntry, GitExportMode, GitExportOptions, GitExportReport,
    MAPPING_FORMAT_VERSION, MAPPING_PROFILE_NAME, default_mapping_path, export_branch_to_git,
};
pub use profile::{
    CANONICALIZATION_ALGORITHM, CanonicalGraphEntry, CanonicalGraphKind, CanonicalManifest,
    CanonicalSyntax, CanonicalTree, GRAPHS_DIR, MANIFEST_FORMAT_VERSION, MANIFEST_PROFILE_NAME,
    MANIFEST_REL_PATH, METADATA_DIR, ReadOptions, WriteOptions, collect_dataset_quads,
    dataset_matches_tree, datasets_isomorphic, import_git_tree, import_git_tree_with,
    read_git_tree, read_git_tree_with, write_git_tree, write_snapshot_git_tree,
};

use crate::Dataset;
use crate::error::{ComponentError, Error};
use std::fmt;
use std::path::{Path, PathBuf};

/// Result alias for [`sparkles::git`](crate::git) operations.
pub type GitResult<T> = std::result::Result<T, GitError>;

/// Typed errors of the canonical git serialization profile and git export.
#[derive(Debug)]
pub enum GitError {
    /// A manifest or mapping file declared a format version this release does not read.
    UnsupportedFormatVersion {
        file: String,
        found: u32,
        supported: u32,
    },
    /// A manifest was malformed, missing required members, or inconsistent.
    InvalidManifest(String),
    /// A path in a manifest or repository escaped the canonical root or used unsafe syntax.
    UnsafePath { path: String, reason: String },
    /// A commit mapping record was malformed or violated bidirectional uniqueness.
    InvalidMapping(String),
    /// An RDF graph file failed to parse or contained a quad for the wrong graph.
    RdfParse { path: String, message: String },
    /// Formatting or RDFC-1.0 canonicalization failed.
    Format(String),
    /// A `git` subprocess exited with a non-zero status or could not be executed.
    GitCommand {
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    /// A commit's historical state could not be reconstructed for per-commit export.
    HistoryUnavailable(String),
    /// An error from the underlying Sparkles store or dataset.
    Store(Error),
    /// A filesystem I/O error.
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
}

impl GitError {
    /// Stable kebab-case error code.
    pub fn code(&self) -> &'static str {
        match self {
            GitError::UnsupportedFormatVersion { .. } => "unsupported-format-version",
            GitError::InvalidManifest(_) => "invalid-manifest",
            GitError::UnsafePath { .. } => "unsafe-path",
            GitError::InvalidMapping(_) => "invalid-mapping",
            GitError::RdfParse { .. } => "rdf-parse",
            GitError::Format(_) => "format-error",
            GitError::GitCommand { .. } => "git-command-failed",
            GitError::HistoryUnavailable(_) => "history-unavailable",
            GitError::Store(_) => "store",
            GitError::Io { .. } => "io",
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        GitError::Io {
            path: Some(path.into()),
            source,
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::UnsupportedFormatVersion {
                file,
                found,
                supported,
            } => write!(
                f,
                "{file} has unsupported format_version {found}; this release reads format_version {supported}"
            ),
            GitError::InvalidManifest(msg) => write!(f, "invalid git manifest: {msg}"),
            GitError::UnsafePath { path, reason } => {
                write!(f, "unsafe path {path:?}: {reason}")
            }
            GitError::InvalidMapping(msg) => write!(f, "invalid git commit mapping: {msg}"),
            GitError::RdfParse { path, message } => {
                write!(f, "failed to parse {path}: {message}")
            }
            GitError::Format(msg) => write!(f, "canonical formatting failed: {msg}"),
            GitError::GitCommand {
                args,
                status,
                stderr,
            } => {
                let code = status
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                let detail = stderr.trim();
                if detail.is_empty() {
                    write!(f, "git {} failed (exit {code})", args.join(" "))
                } else {
                    write!(f, "git {} failed (exit {code}): {detail}", args.join(" "))
                }
            }
            GitError::HistoryUnavailable(msg) => write!(f, "commit history unavailable: {msg}"),
            GitError::Store(e) => write!(f, "{e}"),
            GitError::Io {
                path: Some(p),
                source,
            } => write!(f, "{}: {source}", p.display()),
            GitError::Io { path: None, source } => write!(f, "I/O error: {source}"),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitError::Store(e) => Some(e),
            GitError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<Error> for GitError {
    fn from(e: Error) -> Self {
        GitError::Store(e)
    }
}

impl From<std::io::Error> for GitError {
    fn from(source: std::io::Error) -> Self {
        GitError::Io { path: None, source }
    }
}

impl From<GitError> for Error {
    fn from(e: GitError) -> Self {
        let code = e.code();
        let message = e.to_string();
        Error::Component(Box::new(
            ComponentError::new("git", code, message).with_source(e),
        ))
    }
}

impl Dataset {
    /// Write the dataset's current snapshot as a versioned canonical git serialization tree
    /// under `dir`.
    pub fn write_git_tree(
        &self,
        dir: impl AsRef<Path>,
        options: &WriteOptions,
    ) -> crate::Result<CanonicalManifest> {
        let snap = self.snapshot();
        let prefixes = self.prefixes();
        write_snapshot_git_tree(snap.as_ref(), &prefixes, dir.as_ref(), options)
            .map_err(Error::from)
    }

    /// Read and import a canonical git serialization tree from `dir` into this dataset in
    /// one transaction, preserving cross-graph blank-node identity and prefixes.
    pub fn import_git_tree(
        &self,
        dir: impl AsRef<Path>,
        options: &ReadOptions,
    ) -> crate::Result<crate::commit::Receipt> {
        import_git_tree_with(
            self,
            dir.as_ref(),
            options,
            crate::guard::WriteOptions::default(),
        )
        .map_err(Error::from)
    }

    /// Export this dataset (or a named branch of it via [`GitExportOptions::branch`]) to a
    /// Git repository at `repo_dir`.
    pub fn export_git(
        &self,
        repo_dir: impl AsRef<Path>,
        options: &GitExportOptions,
    ) -> crate::Result<GitExportReport> {
        export_branch_to_git(self, repo_dir.as_ref(), options).map_err(Error::from)
    }
}
