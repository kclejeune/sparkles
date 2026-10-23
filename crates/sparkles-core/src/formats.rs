//! The registry of on-disk formats: every kind of file Sparkles keeps, the version this
//! build writes, and the versions it reads.
//!
//! This table is the one place a format version is declared. The store, `sparkles check`,
//! backup restore, `sparkles migrate`, `GET /$/server` (its `formats` member) and the
//! fixture test (`tests/formats.rs`) read it. The constants of the modules that own a
//! file (for example [`crate::builder::FORMAT_VERSION`]) either come from here or are
//! checked against it by this module's tests, so the two cannot drift apart.
//!
//! # Changing a format
//!
//! Each change is one of four kinds, tried in this order (X12 §4.3):
//!
//! 1. **Additive.** A new member with a default, a flag bit that old readers ignore, a new
//!    optional file. No version changes, and nothing here changes.
//! 2. **Lazy in place.** The reader accepts the old and the new form and writes the new
//!    one the next time it writes the file. Raise the kind's `writes`, add the new
//!    version to `reads`, and raise the dataset's `minimumReader` (see
//!    [`DATASET_READER`](crate::commit::DATASET_READER) and [`crate::commit::require_reader`])
//!    before the first file in the new form is written, so that an older release refuses
//!    the dataset instead of misreading it.
//! 3. **At compaction.** For the generation index: add the new format to
//!    [`READABLE_INDEX_FORMATS`] and raise [`crate::builder::FORMAT_VERSION`]. The next
//!    compaction writes the new format, and older generations stay readable until they
//!    expire.
//! 4. **Explicit migration.** When the old form cannot practically be read, register a
//!    step in [`crate::migrate`] from the old version to the new one, and leave the old
//!    version out of `reads`. The store then refuses the old form with a message that
//!    names `sparkles migrate`.
//!
//! A derived kind ([`Class::Derived`]) never needs a migration: a version this build
//! does not read is rebuilt from the primary files.
//!
//! A settings file ([`SETTINGS`]) changes additively, or by raising its `writes` with an
//! upgrade in its reader, which then rewrites the file at the next settings write. Its
//! `format` member is absent in files written before markers existed, which reads as 1.
//! [`settings_to_vec`] and [`settings_from_slice`] add and check the member.
//!
//! A store refuses a dataset whose settings file is newer than this build writes. It does
//! so at open, after it has the database's lock and before it writes any file, with the
//! same check ([`check_dataset_dir`]) that `sparkles migrate --check` applies. Opening a
//! database and checking it for an upgrade therefore agree, and a refused open leaves
//! every file as it was. The informational files in [`INFORMATIONAL_SETTINGS`] never
//! refuse a dataset.
//!
//! Settings files that a release rewrites through a typed struct drop the members that
//! struct does not know. An additive member of such a file must therefore be safe to lose
//! when an older release rewrites the file, or it needs a `format` bump. `dataset.json`
//! keeps unknown members on a rewrite, and so do the files that the server writes as raw
//! JSON values, such as the assistant settings and the runtime layers.
//!
//! Every version that changes is listed under the **Format** heading of the release's
//! CHANGELOG entry.

use crate::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The version of Sparkles this build is, as recorded in `writtenBy` of `dataset.json`
/// and in backup manifests.
pub const SPARKLES_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a file holds, which decides how a format change is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Data that cannot be recomputed. Readers keep older versions inside the window.
    Primary,
    /// An index rebuilt from primary files when its version is not read here.
    Derived,
    /// A small JSON settings or state file.
    Config,
    /// A file of a backup repository, kept possibly for years.
    Archive,
}

/// One kind of file, with its versions.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileKind {
    /// A short stable name (`wal`, `index`, ...), used in messages and in `--check` output.
    pub id: &'static str,
    /// The files of this kind.
    pub files: &'static str,
    /// Where the version is recorded.
    pub marker: &'static str,
    pub class: Class,
    /// The version this build writes.
    pub writes: u32,
    /// The versions this build reads, ascending. A version 0 means files without a
    /// marker, written before the marker existed.
    pub reads: &'static [u32],
}

impl FileKind {
    /// Whether this build reads version `v`.
    pub fn reads(&self, v: u32) -> bool {
        self.reads.contains(&v)
    }

    /// The versions this build reads, for messages: `2`, `1 and 2`, `0, 1 and 2`.
    pub fn readable(&self) -> String {
        list(self.reads)
    }

    /// Refuse a file of version `found` that this build does not read. `what` names the
    /// file, `written_by` is the release that wrote it if known. A newer version asks for
    /// a newer release. An older one names `sparkles migrate` when a step upgrades it,
    /// and dump and reload otherwise.
    pub fn refuse(&self, what: &str, found: u32, written_by: Option<&str>) -> Error {
        let by = written_by
            .map(|v| format!(", written by Sparkles {v}"))
            .unwrap_or_default();
        let reads = self.readable();
        let s = if self.reads.len() == 1 { "" } else { "s" };
        let head = format!(
            "{what} has {} version {found}{by}; Sparkles {SPARKLES_VERSION} reads version{s} {reads}",
            self.id
        );
        if self.reads.iter().all(|&r| found > r) {
            return Error::Unsupported(format!(
                "{head}. It was written by a newer release: open it with that release or a later one"
            ));
        }
        if crate::migrate::upgrades(self.id, found) {
            return Error::Unsupported(format!(
                "{head}. Upgrade it with `sparkles migrate`, which keeps its history"
            ));
        }
        let dump = written_by
            .map(|v| format!("Sparkles {v}"))
            .unwrap_or_else(|| "the release that wrote it".into());
        Error::Unsupported(format!(
            "{head}. It is older than this release reads: dump it with `sparkles dump` from {dump} and load the dump with `sparkles load`"
        ))
    }

    /// `Ok` when this build reads version `found`, else [`refuse`](Self::refuse).
    pub fn check(&self, what: &str, found: u32, written_by: Option<&str>) -> Result<()> {
        if self.reads(found) {
            Ok(())
        } else {
            Err(self.refuse(what, found, written_by))
        }
    }
}

fn list(v: &[u32]) -> String {
    match v {
        [] => "none".into(),
        [a] => a.to_string(),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(u32::to_string).collect();
            format!("{} and {last}", rest.join(", "))
        }
    }
}

/// The generation index formats this build reads (`format_version` of `meta.json`).
/// [`crate::builder::FORMAT_VERSION`] is the one it writes. A change that is read and
/// rewritten at compaction adds the new format here and keeps the previous one.
pub const READABLE_INDEX_FORMATS: &[u32] = &[2];

/// The write-ahead log version this build writes: a header record with the magic and
/// this version starts every new `wal.log` (see [`crate::store::wal`]). Version 0 is a
/// log without the header, written before it existed.
pub const WAL_VERSION: u32 = 1;

/// `dataset.json`: the dataset id and the reader capabilities a database needs.
pub const DATASET: FileKind = FileKind {
    id: "dataset",
    files: "dataset.json",
    marker: "`format` 1, `minimumReader` (the reader capability the database needs) and `writtenBy`",
    class: Class::Primary,
    writes: crate::commit::DATASET_READER,
    reads: &[1, 2, 3, 4, 5],
};

/// A generation's index: `gen-*/meta.json` and the permutation and vocabulary files.
pub const INDEX: FileKind = FileKind {
    id: "index",
    files: "gen-*/meta.json and the index files",
    marker: "`format_version` of meta.json",
    class: Class::Primary,
    writes: crate::builder::FORMAT_VERSION,
    reads: READABLE_INDEX_FORMATS,
};

/// The sparse vocabulary index of a generation.
pub const VOCAB_INDEX: FileKind = FileKind {
    id: "vocab-index",
    files: "gen-*/vocab.idx",
    marker: "magic `SPKVIDX1`",
    class: Class::Primary,
    writes: 1,
    reads: &[1],
};

/// The numeric column of a generation's vocabulary.
pub const NUMERIC_COLUMN: FileKind = FileKind {
    id: "numeric-column",
    files: "gen-*/vocab.num",
    marker: "magic `SPKVNUM2`",
    class: Class::Primary,
    writes: 2,
    reads: &[2],
};

/// A generation's predicate patterns (X11 Phase 1). Derived from the generation's SPO
/// permutation: a version this build does not read is left unused, queries read the
/// permutations instead, and the next full compaction writes the current version.
pub const PATTERNS: FileKind = FileKind {
    id: "patterns",
    files: "gen-*/patterns.bin",
    marker: "the u32 after the `SPKPATT\\0` magic; another version is not used and is rebuilt at the next full compaction",
    class: Class::Derived,
    writes: crate::patterns::VERSION,
    reads: &[1],
};

/// The terms added since a generation was built.
pub const DELTA_VOCAB: FileKind = FileKind {
    id: "delta-vocab",
    files: "gen-*/delta.vocab",
    marker: "framed header `\\x89SDV` with its version; version 1 is the legacy file without a header",
    class: Class::Primary,
    writes: crate::vocab::delta::VERSION,
    reads: &[1, 2],
};

/// A generation's write-ahead log.
pub const WAL: FileKind = FileKind {
    id: "wal",
    files: "gen-*/wal.log",
    marker: "a 33-byte header record (magic `\\x89SPKWAL\\n` and the version); version 0 is a log without it. Commit-record flag bits in byte 27 are added compatibly. A version 0 log is strictly framed from `walStrictFrom` of its generation's `commit.json`, an additive member",
    class: Class::Primary,
    writes: WAL_VERSION,
    reads: &[0, 1],
};

/// The commit catalog.
pub const COMMITS: FileKind = FileKind {
    id: "commits",
    files: "commits.bin",
    marker: "magic `SPKCMTS\\0` and version 1 in its header; flag bits added compatibly",
    class: Class::Primary,
    writes: 1,
    reads: &[1],
};

/// The change log.
pub const CHANGE_LOG: FileKind = FileKind {
    id: "changelog",
    files: "changelog segments and index",
    marker: "magics `SPKCHGL\\0` and `SPKCHGI\\0` with `FORMAT` in their headers",
    class: Class::Primary,
    writes: 1,
    reads: &[1],
};

/// Commit messages and digests.
pub const ANNOTATIONS: FileKind = FileKind {
    id: "annotations",
    files: "annotations.bin",
    marker: "magic `SPKANNO\\0` with `FORMAT` in its header",
    class: Class::Primary,
    writes: 1,
    reads: &[1],
};

/// A branch generation's link to the logs it inherits, with its base overlay.
pub const LINK: FileKind = FileKind {
    id: "link",
    files: "gen-*/link.json and gen-*/base.delta",
    marker: "`format` of link.json: 1, or 2 with a base overlay",
    class: Class::Primary,
    writes: 2,
    reads: &[1, 2],
};

/// The server catalog: the datasets a data directory serves.
pub const CATALOG: FileKind = FileKind {
    id: "catalog",
    files: "config.json of the data directory",
    marker: "`format`, absent meaning 1",
    class: Class::Primary,
    writes: 1,
    reads: &[1],
};

/// The full-text index.
pub const TEXT: FileKind = FileKind {
    id: "text",
    files: "text/ (Tantivy) and text.json",
    marker: "`FORMAT` of the index metadata; another format is rebuilt on open",
    class: Class::Derived,
    writes: 2,
    reads: &[2],
};

/// The vector index.
pub const VECTOR: FileKind = FileKind {
    id: "vector",
    files: "vector.json and the vector index files",
    marker: "`formatVersion` of vector.json and the `SPKVEC` file version",
    class: Class::Derived,
    writes: 1,
    reads: &[1],
};

/// The spatial index.
pub const GEO: FileKind = FileKind {
    id: "geo",
    files: "geo.json and the spatial index files",
    marker: "`formatVersion` of geo.json, its literals version 2, and the `SPKGEO` file version",
    class: Class::Derived,
    writes: 1,
    reads: &[1],
};

/// The typing of the head that a ShEx write guard keeps between runs.
pub const VALIDATION_TYPING: FileKind = FileKind {
    id: "validation-typing",
    files: "validation-typing.bin",
    marker: "the u32 after the `SPXTYP01` magic; another version is discarded and the typing rebuilt",
    class: Class::Derived,
    writes: 1,
    reads: &[1],
};

/// The spill files of cursor operators that outgrow their memory budget. Only the
/// process that wrote a file reads it, and a start removes what dead processes left.
pub const SPILL: FileKind = FileKind {
    id: "spill",
    files: "the spill files under `--spill-dir` and their SPARKLES-SPILL marker",
    marker: "the u16 after the `SPKSPL01` magic; another version is refused",
    class: Class::Derived,
    writes: 2,
    reads: &[2],
};

/// The layout marker of a backup repository.
pub const BACKUP_LAYOUT: FileKind = FileKind {
    id: "backup-layout",
    files: "sparkles-repo.json of a backup repository",
    marker: "`format`",
    class: Class::Archive,
    writes: 1,
    reads: &[1],
};

/// The blobs of a backup repository.
pub const BACKUP_BLOB: FileKind = FileKind {
    id: "backup-blob",
    files: "blobs of a backup repository",
    marker: "magic `SPKB` and a format byte",
    class: Class::Archive,
    writes: 1,
    reads: &[1],
};

/// The key slots of an encrypted backup repository.
pub const BACKUP_SLOTS: FileKind = FileKind {
    id: "backup-slots",
    files: "key slots of an encrypted backup repository",
    marker: "`format` of each slot",
    class: Class::Archive,
    writes: 2,
    reads: &[1, 2],
};

/// The manifest of a backup, which records the index format of the backed-up
/// generation (`indexFormat`, read with [`INDEX`]) and the release that wrote it.
pub const BACKUP_MANIFEST: FileKind = FileKind {
    id: "backup-manifest",
    files: "backup manifests",
    marker: "`format`, with `indexFormat` and `server.version`",
    class: Class::Archive,
    writes: 1,
    reads: &[1],
};

/// Checked offline migration state, including the originating old sibling.
pub const MIGRATION_STATE: FileKind = FileKind {
    id: "migration-state",
    files: ".migrate-checked at the database or migration-copy root",
    marker: "JSON `format`, with `mode` and `oldDirectory`; bare legacy modes are refused",
    class: Class::Config,
    writes: 1,
    reads: &[1],
};

/// The family prefix of a sealed server runtime secret, including unknown versions.
pub const SEALED_SECRET_FAMILY: &str = "sparkles-sealed-secret/";
/// The exact existing v1 writer prefix, checked against [`RUNTIME_SECRETS`].
pub const SEALED_SECRET_PREFIX: &str = "sparkles-sealed-secret/1 ";

/// Server runtime-secret files. Version 0 means raw plaintext at the server wrapper,
/// never an authenticated `/0` envelope.
pub const RUNTIME_SECRETS: FileKind = FileKind {
    id: "runtime-secrets",
    files: "secrets/NAME in the server data directory",
    marker: "raw plaintext 0, or `sparkles-sealed-secret/1 ` for sealed values",
    class: Class::Primary,
    writes: 1,
    reads: &[0, 1],
};

/// The existing raw generated key, parsed and permission-checked by the server.
pub const SECRETS_KEY: FileKind = FileKind {
    id: "secrets-key",
    files: "secrets.key in the server data directory",
    marker: "unversioned opaque key material",
    class: Class::Primary,
    writes: 0,
    reads: &[0],
};

/// Check the envelope version without authenticating its body. Raw plaintext is 0
/// for the server wrapper; an encrypted `/0` envelope is never supported.
pub fn secret_version(stored: &[u8]) -> Result<u32> {
    let Some(body) = stored.strip_prefix(SEALED_SECRET_FAMILY.as_bytes()) else {
        return Ok(0);
    };
    let malformed = || Error::Corrupt("malformed runtime secret envelope version".into());
    let end = body.iter().position(|b| *b == b' ').ok_or_else(malformed)?;
    let field = &body[..end];
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return Err(malformed());
    }
    let version: u32 = std::str::from_utf8(field)
        .map_err(|_| malformed())?
        .parse()
        .map_err(|_| malformed())?;
    if version != RUNTIME_SECRETS.writes {
        return Err(Error::Unsupported(format!(
            "runtime secret envelope version {version} is not supported; this build reads sealed version {}",
            RUNTIME_SECRETS.writes
        )));
    }
    if !stored.starts_with(SEALED_SECRET_PREFIX.as_bytes()) {
        return Err(malformed());
    }
    Ok(version)
}

/// Every file kind except the settings files, which [`SETTINGS`] lists.
pub const KINDS: &[&FileKind] = &[
    &DATASET,
    &INDEX,
    &VOCAB_INDEX,
    &NUMERIC_COLUMN,
    &PATTERNS,
    &DELTA_VOCAB,
    &WAL,
    &COMMITS,
    &CHANGE_LOG,
    &ANNOTATIONS,
    &LINK,
    &CATALOG,
    &MIGRATION_STATE,
    &RUNTIME_SECRETS,
    &SECRETS_KEY,
    &TEXT,
    &VECTOR,
    &GEO,
    &VALIDATION_TYPING,
    &SPILL,
    &BACKUP_LAYOUT,
    &BACKUP_BLOB,
    &BACKUP_SLOTS,
    &BACKUP_MANIFEST,
];

/// The kind named `id`.
pub fn kind(id: &str) -> Option<&'static FileKind> {
    KINDS.iter().copied().find(|k| k.id == id)
}

// ------------------------------------------------------------------- settings ------

/// Where a settings file lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// A dataset's directory.
    Dataset,
    /// The data directory, next to the catalog's config.json.
    Server,
}

/// A settings or state file, with the member that holds its version and the version
/// this build writes into it.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsFile {
    pub name: &'static str,
    pub scope: Scope,
    /// The member that holds the version. Most files use `format`. Some older files
    /// named theirs after the file, such as `reasoningFormat`.
    pub member: &'static str,
    /// The version this build writes. A file reads every version from 1 up to this one,
    /// and an absent member is 1.
    pub writes: u32,
}

const fn ds(name: &'static str, writes: u32) -> SettingsFile {
    SettingsFile {
        name,
        scope: Scope::Dataset,
        member: "format",
        writes,
    }
}

const fn server(name: &'static str, writes: u32) -> SettingsFile {
    SettingsFile {
        name,
        scope: Scope::Server,
        member: "format",
        writes,
    }
}

const fn named(f: SettingsFile, member: &'static str) -> SettingsFile {
    SettingsFile {
        name: f.name,
        scope: f.scope,
        member,
        writes: f.writes,
    }
}

/// Every settings and state file that carries a version member, by name. A new
/// settings file is registered here and read and written with [`settings_from_slice`]
/// and [`settings_to_vec`], or, when its type declares the member itself, checked with
/// [`check_settings`]. Files that cannot carry a member are in [`UNMARKED_SETTINGS`].
pub const SETTINGS: &[SettingsFile] = &[
    // a dataset's directory
    ds("describe.json", 1),
    ds("compaction.json", 1),
    ds("quota.json", 1),
    ds("changelog.json", 1),
    ds("history.json", 1),
    ds("branches.json", 2),
    ds("branch.json", 1),
    ds("queries.json", 1),
    ds("algo.json", 2),
    ds("validation.json", 2),
    ds("validation-status.json", 1),
    ds("text.json", 1),
    named(ds("reasoning.json", 2), "reasoningFormat"),
    named(ds("rdfs.json", 1), "rdfsFormat"),
    named(ds("origin.json", 1), "originFormat"),
    ds("graphql.json", 1),
    named(ds("restore.json", 1), "restoreFormat"),
    ds("assistant.json", 1),
    ds("memory.json", 1),
    ds("ingest.json", 1),
    ds("asks.json", 1),
    ds("query-suggestions.json", 1),
    ds("maintenance.json", 1),
    ds("review.json", 1),
    ds("graphrag.json", 1),
    named(ds("graphrag/state.json", 1), "version"),
    named(ds("graphrag/ledger.json", 1), "version"),
    // the data directory
    server("config.json", 1),
    server("models.json", 1),
    server("notifications.json", 1),
    server("notifications-state.json", 1),
    server("tokens.json", 1),
    server("sessions.json", 1),
    server("device-grants.json", 1),
    named(server("repositories.json", 1), "version"),
    server("policies.json", 1),
    server("policy-state.json", 1),
    server("runs.json", 1),
    server(".key-epoch.json", 1),
];

/// Dataset settings files that are read when they are readable and ignored with a warning
/// otherwise. A newer version of one of them never refuses a dataset.
pub const INFORMATIONAL_SETTINGS: &[&str] = &["origin.json", "restore.json"];

/// Whether `name` is one of the [`INFORMATIONAL_SETTINGS`].
pub fn informational(name: &str) -> bool {
    INFORMATIONAL_SETTINGS.contains(&name)
}

/// Unpublished branch scratch that existing startup recovery removes by name.
pub(crate) fn transient_branch_directory(name: &str) -> bool {
    name.starts_with(".create-") || name.ends_with(".deleting")
}

/// Physical branch directories, including retained branches, without unpublished
/// scratch. Startup and migration use the same enumeration and transient classification
/// as branch recovery. Directory and entry errors are never silently skipped.
pub(crate) fn branch_directories(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(root.join("branches")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut branches = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && !transient_branch_directory(&entry.file_name().to_string_lossy())
        {
            branches.push(entry.path());
        }
    }
    branches.sort();
    Ok(branches)
}

/// Read-only compatibility preflight before a root open changes dataset metadata.
/// Branch dataset files may be absent, in which case they inherit the root's identity.
pub(crate) fn check_dataset_tree(root: &std::path::Path) -> Result<()> {
    crate::commit::check_dataset_compatibility(root)?;
    check_dataset_dir(root)?;
    for branch in branch_directories(root)? {
        crate::commit::check_dataset_compatibility(&branch).map_err(|e| match e {
            Error::Unsupported(message) => {
                Error::Unsupported(format!("{}: {message}", branch.display()))
            }
            e => e,
        })?;
        check_dataset_dir(&branch)?;
    }
    Ok(())
}

/// Every reason this build refuses the dataset directory `dir` because of a settings
/// file (the registered files of [`Scope::Dataset`] other than the informational ones).
/// A file that does not exist is skipped. One that is not JSON is skipped too, since its
/// own reader reports the damage. A file with a version this build does not read
/// gives an error that names the directory and keeps its variant.
pub fn dataset_dir_refusals(dir: &std::path::Path) -> Vec<Error> {
    let mut out = Vec::new();
    for s in SETTINGS
        .iter()
        .filter(|s| s.scope == Scope::Dataset && !informational(s.name))
    {
        let bytes = match std::fs::read(dir.join(s.name)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                out.push(Error::Io(e));
                continue;
            }
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        match check_settings(s.name, &v) {
            Ok(_) => {}
            Err(Error::Unsupported(m)) => {
                out.push(Error::Unsupported(format!("{}: {m}", dir.display())))
            }
            Err(Error::Corrupt(m)) => out.push(Error::Corrupt(format!("{}: {m}", dir.display()))),
            Err(e) => out.push(e),
        }
    }
    out
}

/// `Ok` when no settings file of the dataset directory `dir` refuses it, else the first
/// of [`dataset_dir_refusals`], preferring one about a newer version.
pub fn check_dataset_dir(dir: &std::path::Path) -> Result<()> {
    let mut all = dataset_dir_refusals(dir);
    match all.iter().position(|e| matches!(e, Error::Unsupported(_))) {
        Some(i) => Err(all.swap_remove(i)),
        None if all.is_empty() => Ok(()),
        None => Err(all.swap_remove(0)),
    }
}

/// Settings and state files without a version member, each with the reason. A breaking
/// change to one of them moves it to a new file name, which older releases do not read.
pub const UNMARKED_SETTINGS: &[(&str, &str)] = &[
    (
        "prefixes.json",
        "a map from prefix to IRI, in which any member name is a legal prefix",
    ),
    ("prefixes-removed.json", "an array of prefix names"),
    ("verify.json", "a map keyed by repository and backup name"),
    (
        "validation-schema.json",
        "a ShExJ document, whose format the ShEx specification defines",
    ),
    (
        "reasoning-generalized.json",
        "a cache that is rebuilt when it does not parse or does not match",
    ),
    (
        "rename.json",
        "a rename intent that the next start finishes, never kept across releases",
    ),
];

/// The registered settings file `name` (a path's file name).
pub fn settings_file(name: &str) -> Option<&'static SettingsFile> {
    SETTINGS.iter().find(|s| s.name == name)
}

fn registered(name: &str) -> &'static SettingsFile {
    match settings_file(name) {
        Some(s) => s,
        None => {
            debug_assert!(false, "{name} is not in formats::SETTINGS");
            const UNKNOWN: SettingsFile = server("unregistered", 1);
            &UNKNOWN
        }
    }
}

/// The version of a settings file's JSON object, from its member: absent is 1. A
/// version newer than this build writes is refused, since the file may mean something
/// this build would misread.
pub fn check_settings(name: &str, v: &serde_json::Value) -> Result<u32> {
    let s = registered(name);
    let member = s.member;
    let found = match v.get(member) {
        None | Some(serde_json::Value::Null) => 1,
        Some(f) => f
            .as_u64()
            .and_then(|f| u32::try_from(f).ok())
            .filter(|&f| f >= 1)
            .ok_or_else(|| Error::Corrupt(format!("{name}: {member} {f} is not a version")))?,
    };
    if found > s.writes {
        return Err(Error::Unsupported(format!(
            "{name} has {member} {found}; Sparkles {SPARKLES_VERSION} reads {member} up to {}. It was written by a newer release",
            s.writes
        )));
    }
    Ok(found)
}

/// Set the version member of a settings file's JSON object to the version this build
/// writes. A value that is not an object is left as it is.
pub fn stamp_settings(name: &str, v: &mut serde_json::Value) {
    let s = registered(name);
    if let Some(o) = v.as_object_mut() {
        o.insert(s.member.into(), s.writes.into());
    }
}

/// Serialize `value` as the settings file `name`: pretty JSON with its version member.
pub fn settings_to_vec<T: Serialize + ?Sized>(name: &str, value: &T) -> Result<Vec<u8>> {
    let mut v = serde_json::to_value(value).map_err(|e| Error::Invalid(format!("{name}: {e}")))?;
    stamp_settings(name, &mut v);
    serde_json::to_vec_pretty(&v).map_err(|e| Error::Invalid(format!("{name}: {e}")))
}

/// [`settings_to_vec`] with the version member set to `format`, for a file whose content
/// needs an older version than the one this build writes at most. A release that reads
/// only that older version can then still read the file. `format` runs from 1 up to the
/// version this build writes.
pub fn settings_to_vec_at<T: Serialize + ?Sized>(
    name: &str,
    value: &T,
    format: u32,
) -> Result<Vec<u8>> {
    let s = registered(name);
    if format < 1 || format > s.writes {
        return Err(Error::Invalid(format!(
            "{name}: cannot write {} {format}; this build writes 1 to {}",
            s.member, s.writes
        )));
    }
    let mut v = serde_json::to_value(value).map_err(|e| Error::Invalid(format!("{name}: {e}")))?;
    if let Some(o) = v.as_object_mut() {
        o.insert(s.member.into(), format.into());
    }
    serde_json::to_vec_pretty(&v).map_err(|e| Error::Invalid(format!("{name}: {e}")))
}

/// Read the settings file `name` from its bytes: check its `format` with
/// [`check_settings`], then deserialize it without that member, so a type that denies
/// unknown members reads it too. Returns the value and the format found.
pub fn settings_from_slice_with_format<T: DeserializeOwned>(
    name: &str,
    bytes: &[u8],
) -> Result<(T, u32)> {
    let mut v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(format!("{name}: {e}")))?;
    let found = check_settings(name, &v)?;
    if let Some(o) = v.as_object_mut() {
        o.remove(registered(name).member);
    }
    let t = serde_json::from_value(v).map_err(|e| Error::Corrupt(format!("{name}: {e}")))?;
    Ok((t, found))
}

/// [`settings_from_slice_with_format`] without the format.
pub fn settings_from_slice<T: DeserializeOwned>(name: &str, bytes: &[u8]) -> Result<T> {
    settings_from_slice_with_format(name, bytes).map(|(t, _)| t)
}

/// The registry as JSON, for `GET /$/server` and `sparkles migrate --check`.
pub fn report() -> serde_json::Value {
    serde_json::json!({
        "sparkles": SPARKLES_VERSION,
        "kinds": KINDS,
        "settings": SETTINGS,
        "unmarkedSettings": UNMARKED_SETTINGS
            .iter()
            .map(|(name, reason)| serde_json::json!({"name": name, "reason": reason}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_secret_reader_windows_are_registered() {
        let secrets = kind("runtime-secrets").expect("runtime secret files must be registered");
        assert_eq!(secrets.writes, 1);
        assert_eq!(secrets.reads, [0, 1]);
        assert_eq!(secrets.class, Class::Primary);
        let key = kind("secrets-key").expect("opaque generated keys must be registered");
        assert_eq!(key.writes, 0);
        assert_eq!(key.reads, [0]);
        assert_eq!(key.class, Class::Primary);
        assert!(settings_file("secrets.key").is_none());
        assert_eq!(
            SEALED_SECRET_PREFIX,
            format!("{SEALED_SECRET_FAMILY}{} ", secrets.writes)
        );
        assert_eq!(secret_version(b"legacy plaintext").unwrap(), 0);
        assert_eq!(secret_version(SEALED_SECRET_PREFIX.as_bytes()).unwrap(), 1);
        for version in ["0", "2", "x", "4294967296", "", "+1", "01"] {
            let bytes = format!("{SEALED_SECRET_FAMILY}{version} body");
            assert!(secret_version(bytes.as_bytes()).is_err());
        }
    }

    /// The module constants agree with the table.
    #[test]
    fn the_table_matches_the_modules() {
        assert_eq!(INDEX.writes, crate::builder::FORMAT_VERSION);
        assert!(INDEX.reads(crate::builder::FORMAT_VERSION));
        assert_eq!(DELTA_VOCAB.writes, crate::vocab::delta::VERSION);
        assert_eq!(PATTERNS.writes, crate::patterns::VERSION);
        assert_eq!(DATASET.writes, crate::commit::DATASET_READER);
        assert_eq!(
            *DATASET.reads.last().unwrap(),
            crate::commit::DATASET_READER
        );
        assert_eq!(VECTOR.writes, crate::vector::config::FORMAT_VERSION);
        assert_eq!(GEO.writes, crate::geo::config::FORMAT_VERSION);
        assert_eq!(SPILL.writes, u32::from(crate::spill::FORMAT_VERSION));
        assert_eq!(
            crate::stored::FORMAT,
            settings_file(crate::stored::FILE).unwrap().writes
        );
        for k in KINDS {
            assert!(k.reads(k.writes), "{} does not read what it writes", k.id);
            assert!(k.reads.is_sorted(), "{}", k.id);
        }
        let mut names: Vec<&str> = SETTINGS.iter().map(|s| s.name).collect();
        names.extend(UNMARKED_SETTINGS.iter().map(|(n, _)| *n));
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "a settings file is listed twice");
    }

    #[test]
    fn refusals_say_what_to_do() {
        let e = INDEX.refuse("gen-0003", 3, Some("0.4.0")).to_string();
        assert!(
            e.contains("index version 3, written by Sparkles 0.4.0"),
            "{e}"
        );
        assert!(e.contains("newer release"), "{e}");
        let e = INDEX.refuse("gen-0003", 1, Some("0.1.0")).to_string();
        assert!(
            e.contains("sparkles dump") && e.contains("Sparkles 0.1.0"),
            "{e}"
        );
        assert_eq!(WAL.readable(), "0 and 1");
        assert_eq!(list(&[0, 1, 2]), "0, 1 and 2");
    }

    #[test]
    fn settings_formats() {
        let name = "describe.json";
        let b = settings_to_vec(name, &serde_json::json!({"a": 1})).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["format"], 1);
        // absent is 1
        let (v, f): (serde_json::Value, u32) =
            settings_from_slice_with_format(name, br#"{"a": 1}"#).unwrap();
        assert_eq!((v["a"].as_u64(), f), (Some(1), 1));
        // the member is removed before deserializing
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Strict {
            a: u32,
        }
        settings_from_slice::<Strict>(name, &b).unwrap();
        // newer is refused
        let e = settings_from_slice::<serde_json::Value>(name, br#"{"format": 9}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("format 9") && e.contains("newer release"), "{e}");
        assert!(settings_from_slice::<serde_json::Value>(name, br#"{"format": "x"}"#).is_err());
    }
}

#[cfg(test)]
mod algo_catalog_registry_tests {
    use super::*;
    #[test]
    fn algo_catalog_writer_is_registered_once() {
        let name = crate::algo::catalog::FILE;
        let entries: Vec<_> = SETTINGS.iter().filter(|s| s.name == name).collect();
        assert_eq!(
            entries.len(),
            1,
            "the catalog must have exactly one registry entry"
        );
        assert_eq!(entries[0].writes, crate::algo::catalog::FORMAT);
        assert_eq!(entries[0].scope, Scope::Dataset);
        let inventory = report();
        let entries: Vec<_> = inventory["settings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["name"] == name)
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["writes"], 2);
    }
}
