//! Reading and validating manifests. A manifest comes from storage that someone else
//! may control: [`validate`] runs before anything is written for a restore.

use crate::cache::version_of;
use crate::error::Result;
use crate::layout::{
    MANIFEST_KIND, MAX_MANIFEST_BYTES, manifest_key, valid_backup_name, valid_backup_path,
    valid_blob_id, valid_generation,
};
use crate::{BackupError, Code, Manifest, Repository};
use futures::TryStreamExt;
use object_store::{GetOptions, ObjectMeta, ObjectStore};
use std::collections::HashSet;

/// Parse a stored manifest: at most [`MAX_MANIFEST_BYTES`], JSON, `kind`
/// `sparkles-backup` and `format` 1, unknown fields ignored. Violations are
/// `422 invalid-backup` naming the field. Does not run [`validate`].
pub fn parse(bytes: &[u8]) -> Result<Manifest> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(BackupError::invalid_backup(
            "manifest",
            format!("larger than {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let m: Manifest =
        serde_json::from_slice(bytes).map_err(|e| BackupError::invalid_backup("manifest", e))?;
    if m.kind != MANIFEST_KIND {
        return Err(BackupError::invalid_backup(
            "kind",
            format!("{:?} is not {MANIFEST_KIND}", m.kind),
        ));
    }
    if m.format != 1 {
        return Err(BackupError::invalid_backup(
            "format",
            format!("{} is not 1", m.format),
        ));
    }
    Ok(m)
}

/// Everything a restore checks before writing (`422 invalid-backup` naming the field;
/// `422 incompatible-format` for `indexFormat != index_format`):
/// * every `path` passes `layout::valid_backup_path`, without duplicates, and the only
///   generation directory named is `generation`;
/// * `size` is the sum of the blob sizes, every blob is at most `piece_bytes`, and
///   every blob id is valid (`layout::valid_blob_id`);
/// * `dataset.id` and `commit` are well formed (`commit.generation` is `generation` or
///   an earlier one, `commit.ref == "commit:<seq>"`), `encryption` is null.
///
/// The content checks that need the downloaded files (`CURRENT` names `generation`;
/// `gen-NNNN/commit.json` has the dataset id and `baseSeq <= commit.seq`) are the
/// restore's.
pub fn validate(m: &Manifest, piece_bytes: u64, index_formats: &[u32]) -> Result<()> {
    let bad = |field: &str, msg: String| Err(BackupError::invalid_backup(field, msg));
    if m.kind != MANIFEST_KIND {
        return bad("kind", format!("{:?} is not {MANIFEST_KIND}", m.kind));
    }
    if m.format != 1 {
        return bad("format", format!("{} is not 1", m.format));
    }
    // a backup restores as written when its index format is inside the window, and the
    // restored dataset then opens under the same rules as a database
    if !index_formats.contains(&m.index_format) {
        let reads = index_formats
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let remedy = if index_formats.iter().all(|&f| m.index_format > f) {
            "restore it with that release or a later one".to_string()
        } else {
            format!(
                "restore it with Sparkles {}, dump the dataset there and load the dump here",
                m.server.version
            )
        };
        return Err(BackupError::new(
            Code::IncompatibleFormat,
            format!(
                "backup uses index format {}, written by Sparkles {}; this build reads {reads}: {remedy}",
                m.index_format, m.server.version
            ),
        ));
    }
    // a feature of the generation that this build does not read, by name
    if let Err(e) =
        sparkles_core::builder::check_features("backup", &m.index_features, Some(&m.server.version))
    {
        return Err(BackupError::new(Code::IncompatibleFormat, e.to_string()));
    }
    if let Some(encryption) = m.encryption.as_ref().filter(|e| !e.is_null()) {
        #[cfg(feature = "encryption")]
        let supported = encryption.as_object().is_some_and(|e| e.len() == 2)
            && encryption.get("scheme").and_then(|e| e.as_str()) == Some("sparkles-repo-v1")
            && encryption
                .get("epoch")
                .and_then(|e| e.as_u64())
                .is_some_and(|e| (1..=u32::MAX as u64).contains(&e));
        #[cfg(not(feature = "encryption"))]
        let supported = {
            let _ = encryption;
            false
        };
        if !supported {
            return bad(
                "encryption",
                "unsupported backup encryption metadata".into(),
            );
        }
    }
    if !valid_backup_name(&m.name) {
        return bad("name", format!("{:?} is not a valid backup name", m.name));
    }
    if m.dataset.id.is_nil() {
        return bad("dataset.id", "the nil UUID".into());
    }
    if m.dataset
        .next_ordinal
        .is_some_and(|n| !(1..=65536).contains(&n))
    {
        return bad("dataset.nextOrdinal", "invalid ordinal reservation".into());
    }
    if let Some(b) = &m.dataset.branch
        && (b.id != m.dataset.id
            || b.dataset_id.is_nil()
            || b.id == b.dataset_id
            || !sparkles_core::branch::valid_name(&b.name)
            || b.name == "main"
            || !(2..=65536).contains(&b.next_ordinal))
    {
        return bad(
            "dataset.branch",
            "invalid branch identity or ordinal reservation".into(),
        );
    }
    if m.dataset.name.is_empty() || m.dataset.name.chars().any(char::is_control) {
        return bad(
            "dataset.name",
            format!("{:?} is not a dataset name", m.dataset.name),
        );
    }
    if !valid_generation(&m.generation) {
        return bad(
            "generation",
            format!("{:?} is not a generation directory", m.generation),
        );
    }
    let c = &m.commit;
    // the head commit was recorded in `generation`, or in an earlier one when the
    // database was compacted after its last commit
    let number = |g: &str| g.strip_prefix("gen-").and_then(|d| d.parse::<u64>().ok());
    if !valid_generation(&c.generation) || number(&c.generation) > number(&m.generation) {
        return bad(
            "commit.generation",
            format!(
                "{:?} is not {:?} or an earlier generation",
                c.generation, m.generation
            ),
        );
    }
    if c.reference != format!("commit:{}", c.seq) {
        return bad(
            "commit.ref",
            format!("{:?} does not name commit {}", c.reference, c.seq),
        );
    }
    if c.parent != c.seq.checked_sub(1) {
        return bad(
            "commit.parent",
            format!("{:?} is not the parent of commit {}", c.parent, c.seq),
        );
    }
    if chrono::DateTime::parse_from_rfc3339(&c.timestamp).is_err() {
        return bad(
            "commit.timestamp",
            format!("{:?} is not an RFC 3339 time", c.timestamp),
        );
    }
    if m.files.is_empty() {
        return bad("files", "empty".into());
    }
    // a feature this build does not know changes what the backup means: refused
    let features = sparkles_core::formats::BACKUP_MANIFEST_FEATURES;
    if let Some(f) = m.requires.iter().find(|f| !features.contains(&f.as_str())) {
        return Err(BackupError::new(
            Code::IncompatibleFormat,
            format!(
                "backup requires the manifest feature {f:?}, written by Sparkles {}; this build reads {}: restore it with that release or a later one",
                m.server.version,
                features.join(", ")
            ),
        ));
    }
    let mut seen = HashSet::with_capacity(m.files.len());
    let mut total = 0u64;
    for (i, f) in m.files.iter().enumerate() {
        let field = |k: &str| format!("files[{i}].{k}");
        if let Some(d) = &f.definition {
            check_definition(m, i, f, d)?;
        } else if !valid_backup_path(&f.path) {
            return bad(&field("path"), format!("{:?} is not allowed", f.path));
        } else if let Some((dir, _)) = f.path.split_once('/')
            && dir != m.generation
        {
            return bad(
                &field("path"),
                format!("{:?} is outside the generation {}", f.path, m.generation),
            );
        }
        if !seen.insert(f.path.as_str()) {
            return bad(&field("path"), format!("{:?} appears twice", f.path));
        }
        if !valid_blob_id(&f.sha256) {
            return bad(&field("sha256"), format!("{:?} is not a SHA-256", f.sha256));
        }
        if f.blobs.is_empty() {
            return bad(&field("blobs"), "empty".into());
        }
        let mut sum = 0u64;
        for (j, b) in f.blobs.iter().enumerate() {
            if !valid_blob_id(&b.id) {
                return bad(
                    &format!("files[{i}].blobs[{j}].id"),
                    format!("{:?} is not a blob id", b.id),
                );
            }
            if b.size > piece_bytes {
                return bad(
                    &format!("files[{i}].blobs[{j}].size"),
                    format!("{} is larger than a piece ({piece_bytes})", b.size),
                );
            }
            sum = sum
                .checked_add(b.size)
                .ok_or_else(|| BackupError::invalid_backup(&field("blobs"), "sizes overflow"))?;
        }
        if sum != f.size {
            return bad(
                &field("size"),
                format!("{} is not the sum of its blob sizes ({sum})", f.size),
            );
        }
        total = total
            .checked_add(f.size)
            .ok_or_else(|| BackupError::invalid_backup("files", "sizes overflow"))?;
    }
    if !seen.contains("CURRENT") {
        return bad("files", "no CURRENT".into());
    }
    Ok(())
}

/// The largest definition file a reader accepts.
pub const MAX_DEFINITION_BYTES: u64 = 1 << 20;

/// A definition entry (`files[i].definition`): announced by `requires`, a meta file of
/// at most [`MAX_DEFINITION_BYTES`] at its kind's path, and a file version this build
/// reads. An unknown kind is refused when it is required, and is skipped by the
/// restore otherwise, once its path is a plain relative one.
fn check_definition(
    m: &Manifest,
    i: usize,
    f: &crate::FileEntry,
    d: &crate::DefinitionRef,
) -> Result<()> {
    let bad =
        |k: &str, msg: String| Err(BackupError::invalid_backup(&format!("files[{i}].{k}"), msg));
    if !m.requires.iter().any(|r| r == "definitions") {
        return Err(BackupError::invalid_backup(
            "requires",
            format!(
                "{:?} is a definition, and `definitions` is not required",
                f.path
            ),
        ));
    }
    if f.kind != sparkles_core::store::FileKind::Meta {
        return bad(
            "kind",
            format!("definition {:?} is not a meta file", f.path),
        );
    }
    if f.size > MAX_DEFINITION_BYTES {
        return bad(
            "size",
            format!(
                "definition {:?} is larger than {MAX_DEFINITION_BYTES} bytes",
                f.path
            ),
        );
    }
    let Some(kind) = sparkles_core::formats::definition_kind(&d.kind) else {
        if d.required {
            return Err(BackupError::new(
                Code::IncompatibleFormat,
                format!(
                    "backup holds a definition of kind {:?} ({}), written by Sparkles {}, which this build does not know: restore it with that release or a later one",
                    d.kind, f.path, m.server.version
                ),
            ));
        }
        let plain = f.path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 64
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        });
        if !plain || f.path.split('/').count() > 4 {
            return bad("path", format!("{:?} is not allowed", f.path));
        }
        return Ok(());
    };
    if kind.name_of(&f.path).is_none() {
        return bad(
            "path",
            format!("{:?} is not a {} definition path", f.path, kind.id),
        );
    }
    if d.format == 0 {
        return bad("definition.format", "0 is not a version".into());
    }
    if d.format > kind.format() {
        return Err(BackupError::new(
            Code::IncompatibleFormat,
            format!(
                "{} has {} format {}, written by Sparkles {}; this build reads up to {}: restore it with that release or a later one",
                f.path,
                kind.file,
                d.format,
                m.server.version,
                kind.format()
            ),
        ));
    }
    Ok(())
}

/// The files of `m` that a restore writes: every file but the definitions of a kind
/// this build does not know and that are not required.
pub fn restored_files(m: &Manifest) -> Vec<crate::FileEntry> {
    m.files
        .iter()
        .filter(|f| {
            f.definition
                .as_ref()
                .is_none_or(|d| sparkles_core::formats::definition_kind(&d.kind).is_some())
        })
        .cloned()
        .collect()
}

/// The total size of a manifest's files: the restored directory's size before the
/// store adds anything (from the file sizes, not the informational `stats`).
pub fn logical_size(m: &Manifest) -> u64 {
    m.files.iter().fold(0u64, |a, f| a.saturating_add(f.size))
}

/// Read the manifest of backup `name`: from the repository's manifest cache when `meta`
/// (a listing entry) matches a cached entry, else with a `GET` (then cached). Returns
/// the manifest, its object metadata, and whether it was fetched with a `GET`.
/// `404 no-such-backup` if it does not exist; `422 invalid-backup` if it is larger than
/// [`MAX_MANIFEST_BYTES`] (checked before its body is read) or does not [`parse`].
pub(crate) async fn fetch(
    repo: &Repository,
    name: &str,
    meta: Option<&ObjectMeta>,
) -> Result<(Manifest, ObjectMeta, bool)> {
    let security = repo.security().await?;
    fetch_with_security(repo, name, meta, &security).await
}

pub(crate) async fn fetch_with_security(
    repo: &Repository,
    name: &str,
    meta: Option<&ObjectMeta>,
    security: &crate::security::Security,
) -> Result<(Manifest, ObjectMeta, bool)> {
    let missing = || {
        BackupError::new(
            Code::NoSuchBackup,
            format!("no backup {name:?} in repository {}", repo.config.name),
        )
    };
    if !valid_backup_name(name) {
        return Err(missing());
    }
    if let Some(meta) = meta {
        if security.encrypted() {
            if let Some(bytes) = repo.cache.get_sealed(name, &version_of(meta), meta.size) {
                // Corrupted local caches are misses, not authoritative repository failures.
                if let Ok(m) = security.open_manifest(name, &bytes) {
                    return Ok((m, meta.clone(), false));
                }
            }
        } else if let Some(m) = repo.cache.get(name, &version_of(meta), meta.size) {
            return Ok((m, meta.clone(), false));
        }
    }
    let got = match repo
        .store
        .get_opts(&manifest_key(name), GetOptions::default())
        .await
    {
        Ok(g) => g,
        Err(object_store::Error::NotFound { .. }) => return Err(missing()),
        Err(e) => return Err(e.into()),
    };
    let limit = security.manifest_limit();
    if got.meta.size > limit {
        return Err(BackupError::invalid_backup(
            "manifest",
            format!("larger than {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let meta = got.meta.clone();
    let mut stream = got.into_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.try_next().await? {
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(BackupError::invalid_backup(
                "manifest",
                "stored-size limit exceeded",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let m = security.open_manifest(name, &bytes)?;
    if security.encrypted() {
        repo.cache
            .put_sealed(name, &version_of(&meta), meta.size, &bytes);
    } else {
        repo.cache.put(name, &version_of(&meta), meta.size, &m);
    }
    Ok((m, meta, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobRef, FileEntry};
    use serde_json::json;

    const PIECE: u64 = 100;

    fn hex64(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn file(path: &str, sizes: &[u64]) -> FileEntry {
        FileEntry {
            path: path.into(),
            kind: sparkles_core::store::FileKind::Immutable,
            size: sizes.iter().sum(),
            sha256: hex64('a'),
            blobs: sizes
                .iter()
                .map(|s| BlobRef {
                    id: hex64('b'),
                    size: *s,
                })
                .collect(),
            definition: None,
        }
    }

    /// A manifest that validates.
    fn good() -> Manifest {
        let mut m: Manifest = serde_json::from_value(json!({
            "format": 1, "kind": "sparkles-backup", "name": "b1",
            "id": "0b6e5c1a-0000-4000-8000-000000000001",
            "repositoryId": "7d0e5c1a-0000-4000-8000-000000000002",
            "dataset": {"name": "ds", "id": "3f1c9a2e-0000-4000-8000-000000000003", "type": "persistent"},
            "commit": {"seq": 4, "parent": 3, "ref": "commit:4", "timestamp": "2026-09-30T14:05:11.990Z",
                       "kind": "update", "inserted": 1, "deleted": 0, "quads": 3,
                       "generation": "gen-0001", "bulk": false, "exact": true},
            "generation": "gen-0001", "indexFormat": 2,
            "created": "2026-09-30T14:05:11.995Z", "completed": "2026-09-30T14:05:12.101Z",
            "millis": 106, "server": {"version": "0.1.0"}, "parent": null,
            "policy": null, "run": null, "note": null, "files": [],
            "stats": {"logicalBytes": 0, "addedBytes": 0, "files": 0, "blobs": 0,
                      "newBlobs": 0, "reusedBlobs": 0},
            "derived": {"text": null}, "encryption": null
        }))
        .unwrap();
        m.files = vec![
            file("CURRENT", &[8]),
            file("commits.bin", &[64]),
            file("gen-0001/spo.dat", &[100, 100, 7]),
            file("gen-0001/wal.log", &[0]),
        ];
        m
    }

    fn field_of(m: &Manifest) -> (Code, String) {
        let e = validate(m, PIECE, &[2]).unwrap_err();
        let body = e.body();
        (e.code(), body["field"].as_str().unwrap_or("").to_string())
    }

    #[test]
    fn parse_refuses_what_it_cannot_read() {
        assert_eq!(parse(b"{}").unwrap_err().code(), Code::InvalidBackup);
        let big = vec![b' '; (MAX_MANIFEST_BYTES + 1) as usize];
        assert_eq!(parse(&big).unwrap_err().code(), Code::InvalidBackup);
        let m = good();
        assert_eq!(parse(&serde_json::to_vec(&m).unwrap()).unwrap(), m);
    }

    #[test]
    fn a_good_manifest_validates() {
        validate(&good(), PIECE, &[2]).unwrap();
        assert_eq!(logical_size(&good()), 8 + 64 + 207);
        // captured after a compaction: the head commit is in the previous generation
        let mut m = good();
        m.generation = "gen-0002".into();
        for f in &mut m.files {
            f.path = f.path.replace("gen-0001/", "gen-0002/");
        }
        validate(&m, PIECE, &[2]).unwrap();
    }

    #[test]
    fn hostile_paths_are_refused() {
        for p in [
            "../x",
            "gen-0001/../../x",
            "/etc/passwd",
            "gen-0002/spo.dat",
            "text/meta.json",
            "sparkles.lock",
            "gen-0001/a/b",
        ] {
            let mut m = good();
            m.files[2].path = p.into();
            assert_eq!(
                field_of(&m),
                (Code::InvalidBackup, "files[2].path".into()),
                "{p}"
            );
        }
        let mut m = good();
        m.files[3].path = "gen-0001/spo.dat".into();
        assert_eq!(field_of(&m), (Code::InvalidBackup, "files[3].path".into()));
        let mut m = good();
        m.files.remove(0);
        assert_eq!(field_of(&m), (Code::InvalidBackup, "files".into()));
    }

    #[test]
    fn sizes_and_ids_are_checked() {
        let mut m = good();
        m.files[2].size += 1;
        assert_eq!(field_of(&m), (Code::InvalidBackup, "files[2].size".into()));
        let mut m = good();
        m.files[2].blobs[0].size = PIECE + 1;
        m.files[2].size += 1;
        assert_eq!(
            field_of(&m),
            (Code::InvalidBackup, "files[2].blobs[0].size".into())
        );
        let mut m = good();
        m.files[1].blobs[0].id = "ABC".into();
        assert_eq!(
            field_of(&m),
            (Code::InvalidBackup, "files[1].blobs[0].id".into())
        );
        let mut m = good();
        m.files[1].blobs[0].id = hex64('B');
        assert_eq!(
            field_of(&m),
            (Code::InvalidBackup, "files[1].blobs[0].id".into())
        );
        let mut m = good();
        m.files[1].sha256 = "x".into();
        assert_eq!(
            field_of(&m),
            (Code::InvalidBackup, "files[1].sha256".into())
        );
        let mut m = good();
        m.files[1].blobs.clear();
        m.files[1].size = 0;
        assert_eq!(field_of(&m), (Code::InvalidBackup, "files[1].blobs".into()));
        // sizes that overflow when summed
        let mut m = good();
        m.files[1].blobs = vec![
            BlobRef {
                id: hex64('c'),
                size: u64::MAX,
            },
            BlobRef {
                id: hex64('c'),
                size: 1,
            },
        ];
        assert_eq!(
            validate(&m, u64::MAX, &[2]).unwrap_err().code(),
            Code::InvalidBackup
        );
    }

    /// A manifest field and a change that breaks it.
    type Case = (&'static str, Box<dyn Fn(&mut Manifest)>);

    #[test]
    fn identity_and_commit_fields_are_checked() {
        let cases: Vec<Case> = vec![
            ("kind", Box::new(|m| m.kind = "other".into())),
            ("format", Box::new(|m| m.format = 2)),
            ("name", Box::new(|m| m.name = "../b".into())),
            ("dataset.id", Box::new(|m| m.dataset.id = uuid::Uuid::nil())),
            ("dataset.name", Box::new(|m| m.dataset.name.clear())),
            ("generation", Box::new(|m| m.generation = "gen-1".into())),
            (
                "commit.generation",
                Box::new(|m| m.commit.generation = "gen-0002".into()),
            ),
            (
                "commit.ref",
                Box::new(|m| m.commit.reference = "commit:5".into()),
            ),
            ("commit.parent", Box::new(|m| m.commit.parent = None)),
            (
                "commit.timestamp",
                Box::new(|m| m.commit.timestamp = "yesterday".into()),
            ),
            (
                "encryption",
                Box::new(|m| m.encryption = Some(json!({"scheme": "age"}))),
            ),
        ];
        for (field, f) in cases {
            let mut m = good();
            f(&mut m);
            assert_eq!(field_of(&m), (Code::InvalidBackup, field.into()), "{field}");
        }
        let mut m = good();
        m.commit.seq = 0;
        m.commit.reference = "commit:0".into();
        m.commit.parent = None;
        validate(&m, PIECE, &[2]).unwrap();
    }

    /// A backup of a generation with an index feature this build does not read is
    /// refused before anything is written, naming the feature (spec X11 §7.3).
    #[test]
    fn an_unknown_index_feature_is_incompatible() {
        let mut m = good();
        m.index_format = 4;
        m.index_features = vec!["fsst-vocab".into()];
        validate(&m, PIECE, sparkles_core::formats::READABLE_INDEX_FORMATS).unwrap();
        m.index_features.push("zz-future".into());
        let e = validate(&m, PIECE, sparkles_core::formats::READABLE_INDEX_FORMATS).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat);
        assert!(e.to_string().contains("`zz-future`"), "{e}");
    }

    #[test]
    fn a_newer_index_format_is_incompatible() {
        let mut m = good();
        m.index_format = sparkles_core::builder::FORMAT_VERSION + 1;
        let e = validate(&m, PIECE, sparkles_core::formats::READABLE_INDEX_FORMATS).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat);
        assert_eq!(e.http_status(), 422);
        let v = sparkles_core::builder::FORMAT_VERSION;
        let reads = sparkles_core::formats::READABLE_INDEX_FORMATS
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            e.message()
                .contains(&format!("index format {}, written by Sparkles", v + 1))
                && e.message().contains(&format!("this build reads {reads}:")),
            "{e}"
        );
    }
    /// `good()` with view definitions `names`, announced by `requires`.
    fn with_views(names: &[&str]) -> Manifest {
        let mut m = good();
        m.requires = vec!["definitions".into()];
        for n in names {
            let mut f = file(&format!("views/{n}/view.json"), &[40]);
            f.kind = sparkles_core::store::FileKind::Meta;
            f.definition = Some(crate::DefinitionRef {
                kind: "view".into(),
                format: 1,
                required: true,
            });
            m.files.push(f);
        }
        m
    }

    #[test]
    fn definition_entries_validate() {
        let m = with_views(&["a", "b-2"]);
        validate(&m, PIECE, &[2]).unwrap();
        assert_eq!(restored_files(&m).len(), m.files.len());
        // the serialized entry carries its kind and version, and no default member
        let v = serde_json::to_value(&m.files[4]).unwrap();
        assert_eq!(v["definition"], json!({"kind": "view", "format": 1}));
        // a manifest without definitions serializes as before
        let v = serde_json::to_value(good()).unwrap();
        assert!(v.get("requires").is_none() && v["files"][0].get("definition").is_none());
    }

    #[test]
    fn hostile_definition_entries_are_refused() {
        let n = good().files.len();
        let cases: Vec<Case> = vec![
            (
                "traversal",
                Box::new(move |m| m.files[n].path = "views/../../etc/view.json".into()),
            ),
            (
                "nested",
                Box::new(move |m| m.files[n].path = "views/a/b/view.json".into()),
            ),
            (
                "other file",
                Box::new(move |m| m.files[n].path = "views/a/paused.json".into()),
            ),
            (
                "absolute",
                Box::new(move |m| m.files[n].path = "/views/a/view.json".into()),
            ),
            (
                "generation",
                Box::new(move |m| m.files[n].path = "gen-0001/view.json".into()),
            ),
            (
                "duplicate",
                Box::new(move |m| m.files[n + 1].path = "views/a/view.json".into()),
            ),
            (
                "not meta",
                Box::new(move |m| m.files[n].kind = sparkles_core::store::FileKind::Immutable),
            ),
            (
                "too large",
                Box::new(move |m| {
                    m.files[n].size = MAX_DEFINITION_BYTES + 1;
                    m.files[n].blobs = vec![BlobRef {
                        id: hex64('b'),
                        size: MAX_DEFINITION_BYTES + 1,
                    }];
                }),
            ),
            (
                "version 0",
                Box::new(move |m| m.files[n].definition.as_mut().unwrap().format = 0),
            ),
            ("unannounced", Box::new(|m| m.requires.clear())),
        ];
        for (what, f) in cases {
            let mut m = with_views(&["a", "b"]);
            f(&mut m);
            let e = validate(&m, MAX_DEFINITION_BYTES + 1, &[2]).unwrap_err();
            assert_eq!(e.code(), Code::InvalidBackup, "{what}: {e}");
        }
        // an older reader ignores `definition` and `requires`, so it sees a plain file
        // path that it does not allow, and refuses the backup before writing anything
        let mut m = with_views(&["a"]);
        m.files[n].definition = None;
        assert_eq!(
            field_of(&m),
            (Code::InvalidBackup, format!("files[{n}].path"))
        );
    }

    #[test]
    fn unknown_definition_semantics_are_refused_or_skipped() {
        let n = good().files.len();
        // a newer definition file version
        let mut m = with_views(&["a"]);
        m.files[n].definition.as_mut().unwrap().format = 2;
        let e = validate(&m, PIECE, &[2]).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat, "{e}");
        assert!(e.message().contains("views/a/view.json"), "{e}");
        // an unknown required kind
        let mut m = with_views(&["a"]);
        m.files[n].path = "analyses/a/analysis.json".into();
        m.files[n].definition.as_mut().unwrap().kind = "analysis".into();
        let e = validate(&m, PIECE, &[2]).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat, "{e}");
        assert!(e.message().contains("\"analysis\""), "{e}");
        // the same kind, not required: validated as a plain path and left out
        m.files[n].definition.as_mut().unwrap().required = false;
        validate(&m, PIECE, &[2]).unwrap();
        assert_eq!(restored_files(&m).len(), n);
        m.files[n].path = "analyses/../x".into();
        assert_eq!(
            validate(&m, PIECE, &[2]).unwrap_err().code(),
            Code::InvalidBackup
        );
        // an unknown manifest feature
        let mut m = with_views(&["a"]);
        m.requires.push("retained-history".into());
        let e = validate(&m, PIECE, &[2]).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat, "{e}");
        assert!(e.message().contains("retained-history"), "{e}");
    }

    #[test]
    fn branch_provenance_is_validated_without_breaking_legacy_manifests() {
        let mut m = good();
        assert!(m.dataset.branch.is_none());
        assert!(m.dataset.next_ordinal.is_none());
        for next in [0, 65_537] {
            m.dataset.next_ordinal = Some(next);
            assert_eq!(field_of(&m).1, "dataset.nextOrdinal");
        }
        for next in [1, 65_536] {
            m.dataset.next_ordinal = Some(next);
            validate(&m, PIECE, &[2]).unwrap();
        }
        m.dataset.next_ordinal = None;
        m.dataset.branch = Some(sparkles_core::store::BackupBranch {
            dataset_id: uuid::Uuid::new_v4(),
            id: m.dataset.id,
            name: "work".into(),
            next_ordinal: 2,
        });
        validate(&m, PIECE, &[2]).unwrap();
        for next in [0, 1, 65_537] {
            m.dataset.branch.as_mut().unwrap().next_ordinal = next;
            assert_eq!(field_of(&m).1, "dataset.branch");
        }
        m.dataset.branch.as_mut().unwrap().next_ordinal = 2;
        m.dataset.branch.as_mut().unwrap().name = "main".into();
        assert_eq!(field_of(&m).1, "dataset.branch");
        m.dataset.branch.as_mut().unwrap().name = "work".into();
        m.dataset.branch.as_mut().unwrap().id = uuid::Uuid::new_v4();
        assert_eq!(field_of(&m).1, "dataset.branch");
    }
}
