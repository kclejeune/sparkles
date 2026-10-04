//! Branches and merges ([F09](../../../docs/specs/F09-branches-and-merges.md)).
//!
//! A branch is a named, writable line of commits that starts from a commit of another
//! branch. Every persistent dataset has the branch `main`, which is the dataset's own
//! store. Other branches live under `<dataset>/branches/<branch id>/`, each a store with
//! its own write-ahead log, commits, history and writer lock. A new branch writes no
//! index: its first generation is *linked*, which means it reads the base files of the
//! generation that owns its starting commit and replays that generation's log up to the
//! commit. The branch owns a full index only after its first rebuild.
//!
//! This module holds the public types of the branch API. The engine is in
//! `store/branching.rs` (the branch table, creation, deletion, holds and merge bases)
//! and `store/merge.rs` (three-way merges).

use crate::commit::CommitInfo;
use crate::error::{Error, Result};
use oxrdf::{GraphName, NamedNode, Term};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// The name of every dataset's own branch.
pub const MAIN: &str = "main";

/// The most branches a dataset may have, `main` included, unless the store options
/// say otherwise.
pub const DEFAULT_MAX_BRANCHES: usize = 64;

/// The most log segments a linked generation chains before a new branch is built as its
/// own generation.
pub const DEFAULT_MAX_BRANCH_DEPTH: usize = 4;

/// Bits of a stored blank node's payload that count nodes within one branch. The bits
/// above them hold the branch's ordinal.
pub const BNODE_COUNTER_BITS: u32 = 43;

/// The first blank-node payload of the branch with `ordinal`.
pub fn bnode_range_start(ordinal: u16) -> u64 {
    (ordinal as u64) << BNODE_COUNTER_BITS
}

/// The branch ordinal of a stored blank node's payload.
pub fn bnode_ordinal(payload: u64) -> u64 {
    payload >> BNODE_COUNTER_BITS
}

/// Whether `name` is a valid branch name: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, with at
/// least one letter, and not `head`.
pub fn valid_name(name: &str) -> bool {
    crate::history::valid_name(name)
        && name.bytes().any(|b| b.is_ascii_alphabetic())
        && name != "head"
}

/// A commit of some branch: the branch id and the seq.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRef {
    pub branch_id: uuid::Uuid,
    pub seq: u64,
}

/// A commit named by its branch name (`None` when that branch was deleted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedCommitRef {
    pub branch: Option<String>,
    pub branch_id: uuid::Uuid,
    pub seq: u64,
}

/// What a branch holds on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchStorage {
    /// the branch still reads an upstream generation's base files
    pub linked: bool,
    /// bytes of the branch's own directory
    pub own_bytes: u64,
    /// bytes of upstream generations that are no longer current there and that this
    /// branch's link keeps
    pub held_bytes: u64,
    /// the current generation of the branch (`gen-0000` while linked)
    pub generation: String,
}

/// One branch, as listings show it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub name: String,
    pub id: uuid::Uuid,
    pub ordinal: u16,
    /// the head commit (`None` when the branch's files cannot be read)
    #[serde(skip)]
    pub head: Option<CommitInfo>,
    /// the commit the branch started from (`None` for `main`)
    pub from: Option<NamedCommitRef>,
    /// the branch it was created from (`None` for `main`)
    pub upstream: Option<String>,
    /// the merge base with the upstream
    pub merge_base: Option<NamedCommitRef>,
    /// own commits the upstream does not descend from, and the reverse
    pub ahead: u64,
    pub behind: u64,
    pub protected: bool,
    pub note: Option<String>,
    pub created_ms: i64,
    pub storage: BranchStorage,
    /// the branch's directory is listed but missing or unreadable
    pub broken: bool,
}

/// Options of [`Store::create_branch`](crate::store::Store::create_branch).
#[derive(Clone, Debug)]
pub struct BranchOptions {
    /// the branch to start from
    pub from: String,
    /// the commit of `from` to start at
    pub at: crate::history::At,
    pub protected: bool,
    pub note: Option<String>,
}

impl Default for BranchOptions {
    fn default() -> Self {
        BranchOptions {
            from: MAIN.to_string(),
            at: crate::history::At::Head,
            protected: false,
            note: None,
        }
    }
}

/// What counts as one value when both sides changed it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictScope {
    /// a graph, a subject and a predicate
    #[default]
    Cell,
    /// a graph and a subject
    Subject,
    /// never a conflict: the quad-level three-way result everywhere
    Quad,
}

impl ConflictScope {
    pub fn parse(s: &str) -> Option<ConflictScope> {
        match s {
            "cell" => Some(ConflictScope::Cell),
            "subject" => Some(ConflictScope::Subject),
            "quad" => Some(ConflictScope::Quad),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ConflictScope::Cell => "cell",
            ConflictScope::Subject => "subject",
            ConflictScope::Quad => "quad",
        }
    }
}

/// How a conflict is resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Take {
    /// keep the target's state of the group
    Ours,
    /// take the source's state of the group
    Theirs,
    /// put the group back as it was at the merge base
    Base,
    /// the quad-level result: both sides' changes
    Union,
    /// set the cell to these objects
    Objects(Vec<Term>),
}

impl Take {
    pub fn parse(s: &str) -> Option<Take> {
        match s {
            "ours" => Some(Take::Ours),
            "theirs" => Some(Take::Theirs),
            "base" => Some(Take::Base),
            "union" => Some(Take::Union),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Take::Ours => "ours",
            Take::Theirs => "theirs",
            Take::Base => "base",
            Take::Union => "union",
            Take::Objects(_) => "objects",
        }
    }
}

/// A choice for the conflicts of a graph, a subject or a cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub graph: GraphName,
    pub subject: Option<Term>,
    pub predicate: Option<NamedNode>,
    pub take: Take,
}

/// Options of [`Store::merge`](crate::store::Store::merge).
#[derive(Clone, Default)]
pub struct MergeOptions {
    /// refuse anything but a fast-forward
    pub ff_only: bool,
    pub scope: ConflictScope,
    /// the rule for every conflict no resolution covers (`None`: fail)
    pub on_conflict: Option<Take>,
    pub resolutions: Vec<Resolution>,
    /// the source head the caller saw
    pub expect_source: Option<u64>,
    /// the target head the caller saw
    pub expect_target: Option<u64>,
    /// the merge base to use when there are several
    pub base: Option<CommitRef>,
    /// merge the inferred graph like any other graph
    pub include_inferences: bool,
    /// the commit's message and the write's options (guard, deadline, dry run)
    pub write: crate::guard::WriteOptions,
    /// the most quads the two change sets may hold (0: no limit)
    pub max_quads: u64,
    /// the most conflict cells a report lists (0: 100)
    pub limit: usize,
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<Instant>,
}

/// The objects of one side of a conflicting group, in N-Triples.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConflictCell {
    /// the graph (`None`: the default graph)
    pub graph: Option<String>,
    pub subject: String,
    /// `None` for a conflict of the subject scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    /// the group's quads at the base, in ours and in theirs: objects for a cell,
    /// `predicate object` pairs for a subject
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

/// Conflicts counted by graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphConflicts {
    pub graph: Option<String>,
    pub conflicts: u64,
}

/// The conflicts that stopped a merge.
#[derive(Clone, Debug, Serialize)]
pub struct ConflictReport {
    pub error: String,
    pub code: &'static str,
    pub source: NamedCommitRef,
    pub target: NamedCommitRef,
    pub base: Option<NamedCommitRef>,
    pub scope: ConflictScope,
    pub conflicts: u64,
    pub truncated: bool,
    pub graphs: Vec<GraphConflicts>,
    pub cells: Vec<ConflictCell>,
}

/// What a merge did, or would do.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeReport {
    pub merged: bool,
    pub up_to_date: bool,
    pub fast_forward: bool,
    pub source: NamedCommitRef,
    pub target: NamedCommitRef,
    pub base: Option<NamedCommitRef>,
    pub inserted: u64,
    pub deleted: u64,
    /// conflicting groups found, and how many of them resolutions or the rule settled
    pub conflicts_found: u64,
    pub conflicts_resolved: u64,
    /// the merge commit
    #[serde(skip)]
    pub commit: Option<crate::commit::Receipt>,
    /// changes of the inferred graph left out
    pub inferences_excluded: Option<u64>,
    /// the conflicts that remain (previews only)
    #[serde(skip)]
    pub conflicts: Option<ConflictReport>,
}

/// The outcome of [`Store::merge`](crate::store::Store::merge).
#[derive(Clone, Debug)]
pub enum MergeOutcome {
    /// the source's changes are already in the target: nothing was written
    UpToDate(MergeReport),
    Merged(MergeReport),
    /// conflicts remain: nothing was written
    Conflicts(Box<ConflictReport>),
}

/// The class of a branch error, which decides its HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchErrorKind {
    /// 400
    Invalid,
    /// 403
    Forbidden,
    /// 404
    NotFound,
    /// 409
    Conflict,
    /// 410
    Gone,
    /// 501
    Unsupported,
}

/// An error of the branch API, with its stable code.
#[derive(Clone, Debug)]
pub struct BranchError {
    pub kind: BranchErrorKind,
    /// `no-such-branch`, `branch-exists`, `branch-limit`, `unmerged`, `has-children`,
    /// `branch-protected`, `merge-conflict`, `not-fast-forward`, `head-moved`,
    /// `ambiguous-merge-base`, `merge-base-gone`, `invalid-branch`, `invalid-merge`,
    /// `branches-unsupported` or `inherited-commit`
    pub code: &'static str,
    pub message: String,
    /// the conflicts of a `merge-conflict`
    pub conflicts: Option<Box<ConflictReport>>,
    /// the candidates of an `ambiguous-merge-base`
    pub candidates: Vec<NamedCommitRef>,
    /// the commit an `inherited-commit` read should be served from
    pub inherited: Option<CommitRef>,
}

impl BranchError {
    /// The error of `kind` with `code`.
    pub fn error(kind: BranchErrorKind, code: &'static str, message: impl Into<String>) -> Error {
        Error::Branch(Box::new(BranchError {
            kind,
            code,
            message: message.into(),
            conflicts: None,
            candidates: Vec::new(),
            inherited: None,
        }))
    }

    /// The HTTP status of the error.
    pub fn status(&self) -> u16 {
        match self.kind {
            BranchErrorKind::Invalid => 400,
            BranchErrorKind::Forbidden => 403,
            BranchErrorKind::NotFound => 404,
            BranchErrorKind::Conflict => 409,
            BranchErrorKind::Gone => 410,
            BranchErrorKind::Unsupported => 501,
        }
    }
}

impl std::fmt::Display for BranchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) fn no_such_branch(name: &str) -> Error {
    BranchError::error(
        BranchErrorKind::NotFound,
        "no-such-branch",
        format!("no such branch: {name}"),
    )
}

pub(crate) fn invalid_branch(msg: impl Into<String>) -> Error {
    BranchError::error(BranchErrorKind::Invalid, "invalid-branch", msg)
}

pub(crate) fn invalid_merge(msg: impl Into<String>) -> Error {
    BranchError::error(BranchErrorKind::Invalid, "invalid-merge", msg)
}

pub(crate) fn conflict(code: &'static str, msg: impl Into<String>) -> Error {
    BranchError::error(BranchErrorKind::Conflict, code, msg)
}

pub(crate) fn protected(name: &str) -> Error {
    BranchError::error(
        BranchErrorKind::Forbidden,
        "branch-protected",
        format!("branch {name} is protected: it takes changes through merges only"),
    )
}

/// Check a branch name for creation.
pub(crate) fn check_name(name: &str) -> Result<()> {
    if name == MAIN {
        return Err(conflict(
            "branch-exists",
            "branch main exists in every dataset",
        ));
    }
    if !valid_name(name) {
        return Err(invalid_branch(format!(
            "invalid branch name {name:?}: letters, digits, '.', '_' and '-', 1 to 64, starting with a letter or digit, with at least one letter, and not 'head'"
        )));
    }
    Ok(())
}

/// The commit that an [`Error::Branch`] with code `inherited-commit` names: a branch
/// store asked for a commit it shares with its upstream.
pub fn inherited_commit(e: &Error) -> Option<CommitRef> {
    match e {
        Error::Branch(b) if b.code == "inherited-commit" => b.inherited,
        _ => None,
    }
}
