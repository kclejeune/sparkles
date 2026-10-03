//! Reading and validating manifests. A manifest comes from storage that someone else
//! may control: [`validate`] runs before anything is written for a restore.

use crate::cache::version_of;
use crate::error::Result;
use crate::layout::{
    MANIFEST_KIND, MAX_MANIFEST_BYTES, manifest_key, valid_backup_name, valid_backup_path,
    valid_blob_id, valid_generation,
};
use crate::{BackupError, Code, Manifest, Repository};
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
pub fn validate(m: &Manifest, piece_bytes: u64, index_format: u32) -> Result<()> {
    let bad = |field: &str, msg: String| Err(BackupError::invalid_backup(field, msg));
    if m.kind != MANIFEST_KIND {
        return bad("kind", format!("{:?} is not {MANIFEST_KIND}", m.kind));
    }
    if m.format != 1 {
        return bad("format", format!("{} is not 1", m.format));
    }
    if m.index_format != index_format {
        return Err(BackupError::new(
            Code::IncompatibleFormat,
            format!(
                "backup uses index format {}; this build reads {index_format}",
                m.index_format
            ),
        ));
    }
    if m.encryption.as_ref().is_some_and(|e| !e.is_null()) {
        return bad(
            "encryption",
            "the backup is encrypted; this build cannot read it".into(),
        );
    }
    if !valid_backup_name(&m.name) {
        return bad("name", format!("{:?} is not a valid backup name", m.name));
    }
    if m.dataset.id.is_nil() {
        return bad("dataset.id", "the nil UUID".into());
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
    let mut seen = HashSet::with_capacity(m.files.len());
    let mut total = 0u64;
    for (i, f) in m.files.iter().enumerate() {
        let field = |k: &str| format!("files[{i}].{k}");
        if !valid_backup_path(&f.path) {
            return bad(&field("path"), format!("{:?} is not allowed", f.path));
        }
        if let Some((dir, _)) = f.path.split_once('/')
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
    let missing = || {
        BackupError::new(
            Code::NoSuchBackup,
            format!("no backup {name:?} in repository {}", repo.config.name),
        )
    };
    if !valid_backup_name(name) {
        return Err(missing());
    }
    if let Some(meta) = meta
        && let Some(m) = repo.cache.get(name, &version_of(meta), meta.size)
    {
        return Ok((m, meta.clone(), false));
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
    if got.meta.size > MAX_MANIFEST_BYTES {
        return Err(BackupError::invalid_backup(
            "manifest",
            format!("larger than {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let meta = got.meta.clone();
    let bytes = got.bytes().await?;
    let m = parse(&bytes)?;
    repo.cache.put(name, &version_of(&meta), meta.size, &m);
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
        let e = validate(m, PIECE, 2).unwrap_err();
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
        validate(&good(), PIECE, 2).unwrap();
        assert_eq!(logical_size(&good()), 8 + 64 + 207);
        // captured after a compaction: the head commit is in the previous generation
        let mut m = good();
        m.generation = "gen-0002".into();
        for f in &mut m.files {
            f.path = f.path.replace("gen-0001/", "gen-0002/");
        }
        validate(&m, PIECE, 2).unwrap();
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
            validate(&m, u64::MAX, 2).unwrap_err().code(),
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
        validate(&m, PIECE, 2).unwrap();
    }

    #[test]
    fn a_newer_index_format_is_incompatible() {
        let mut m = good();
        m.index_format = sparkles_core::builder::FORMAT_VERSION + 1;
        let e = validate(&m, PIECE, sparkles_core::builder::FORMAT_VERSION).unwrap_err();
        assert_eq!(e.code(), Code::IncompatibleFormat);
        assert_eq!(e.http_status(), 422);
        let v = sparkles_core::builder::FORMAT_VERSION;
        assert!(
            e.message()
                .contains(&format!("index format {}; this build reads {v}", v + 1)),
            "{e}"
        );
    }
}
