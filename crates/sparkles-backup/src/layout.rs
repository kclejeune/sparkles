//! The repository layout (format 1), the repository marker, and the name grammars.
//!
//! ```text
//! <prefix>/
//!   sparkles-repo.json            marker, created once (conditional create)
//!   blobs/<hh>/<64 hex>           content blobs, immutable; <hh> = first two hex digits
//!   backups/<name>.json           manifests, immutable, created last (conditional create)
//!   locks/<uuid>.json             lease objects
//!   probe/<uuid>                  connection-test objects
//!   gc/last.json                  the last GC report (overwritten)
//! ```
//!
//! Keys here are relative to the repository root; the backend wraps the store so that
//! they land under the configured prefix.

use crate::error::{BackupError, Code, Result};
use object_store::path::Path as Key;
use serde::{Deserialize, Serialize};
use serde_json::Value as J;
use uuid::Uuid;

/// Repository format written and read by this build.
pub const FORMAT: u32 = 1;
/// `kind` of the marker.
pub const MARKER_KIND: &str = "sparkles-backup-repository";
/// `kind` of a manifest.
pub const MANIFEST_KIND: &str = "sparkles-backup";
/// The marker object.
pub const MARKER: &str = "sparkles-repo.json";
pub const BLOBS: &str = "blobs";
pub const BACKUPS: &str = "backups";
pub const LOCKS: &str = "locks";
pub const PROBE: &str = "probe";
pub const GC_LAST: &str = "gc/last.json";

/// Piece size of new repositories (fixed in the marker at initialization).
pub const PIECE_BYTES: u64 = 32 << 20;
/// Largest manifest a reader accepts.
pub const MAX_MANIFEST_BYTES: u64 = 16 << 20;
/// Segments of an append-only file before it is stored from scratch.
pub const MAX_SEGMENTS: usize = 64;

pub fn marker_key() -> Key {
    Key::from(MARKER)
}

/// `blobs/<hh>/<id>` (`id` must be a valid blob id).
pub fn blob_key(id: &str) -> Key {
    Key::from(format!("{BLOBS}/{}/{id}", &id[..2]))
}

/// The blob id of a `blobs/<hh>/<id>` key, if it is one.
pub fn blob_id_of(key: &Key) -> Option<&str> {
    let s = key.as_ref().strip_prefix(BLOBS)?.strip_prefix('/')?;
    let (hh, id) = s.split_once('/')?;
    (valid_blob_id(id) && id.starts_with(hh)).then_some(id)
}

/// `backups/<name>.json` (`name` must be a valid backup name).
pub fn manifest_key(name: &str) -> Key {
    Key::from(format!("{BACKUPS}/{name}.json"))
}

/// The backup name of a `backups/<name>.json` key, if it is one.
pub fn backup_name_of(key: &Key) -> Option<&str> {
    let s = key.as_ref().strip_prefix(BACKUPS)?.strip_prefix('/')?;
    let name = s.strip_suffix(".json")?;
    valid_backup_name(name).then_some(name)
}

/// `locks/<id>.json`
pub fn lock_key(id: &str) -> Key {
    Key::from(format!("{LOCKS}/{id}.json"))
}

/// The lock id of a `locks/<id>.json` key, if it is one.
pub fn lock_id_of(key: &Key) -> Option<&str> {
    let s = key.as_ref().strip_prefix(LOCKS)?.strip_prefix('/')?;
    let id = s.strip_suffix(".json")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// `probe/<uuid>`
pub fn probe_key(id: Uuid) -> Key {
    Key::from(format!("{PROBE}/{id}"))
}

pub fn gc_last_key() -> Key {
    Key::from(GC_LAST)
}

// --------------------------------------------------------------------- grammars ------

/// Repository and policy names: `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn valid_repo_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

/// Backup names: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}` (the named-snapshot grammar).
pub fn valid_backup_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Blob ids: 64 lowercase hex digits.
pub fn valid_blob_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Files at the database root that a backup may hold.
pub const ROOT_FILES: [&str; 14] = [
    "CURRENT",
    "dataset.json",
    "commits.bin",
    "annotations.bin",
    "prefixes.json",
    "text.json",
    "geo.json",
    "vector.json",
    "reasoning.json",
    "origin.json",
    "validation.json",
    "validation-shapes.ttl",
    "validation-schema.shex",
    "validation-schema.json",
];

/// A manifest file path: one of [`ROOT_FILES`], or `gen-NNNN/<file>` with 4 to 8 digits
/// and a file name of `[A-Za-z0-9._-]{1,64}` other than `.` and `..`. Anything else
/// (absolute paths, `..`, more levels) is refused before a path is joined.
pub fn valid_backup_path(p: &str) -> bool {
    if ROOT_FILES.contains(&p) {
        return true;
    }
    let Some((dir, file)) = p.split_once('/') else {
        return false;
    };
    valid_generation(dir)
        && !file.is_empty()
        && file.len() <= 64
        && file != "."
        && file != ".."
        && file
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A generation directory name: `gen-` and 4 to 8 digits.
pub fn valid_generation(s: &str) -> bool {
    s.strip_prefix("gen-")
        .is_some_and(|d| (4..=8).contains(&d.len()) && d.bytes().all(|c| c.is_ascii_digit()))
}

/// `YYYYMMDDtHHMMSSz` of an instant (UTC), as used in default and template names.
pub fn time_tag(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y%m%dt%H%M%Sz").to_string()
}

/// The default name of a backup of `dataset` at `t`: `{dataset}-{time}`, with the
/// dataset part shortened (and characters outside the grammar replaced) so the name
/// stays valid.
pub fn default_backup_name(dataset: &str, t: chrono::DateTime<chrono::Utc>) -> String {
    let tag = time_tag(t);
    let room = 64 - tag.len() - 1;
    let mut ds: String = dataset
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(room)
        .collect();
    if !ds.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        ds.insert(0, 'b');
        ds.truncate(room);
    }
    format!("{ds}-{tag}")
}

// ----------------------------------------------------------------------- marker ------

/// `sparkles-repo.json`: written once at initialization. Unknown fields are ignored, so
/// additive changes stay format 1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub format: u32,
    pub kind: String,
    pub id: Uuid,
    pub created: String,
    /// `sparkles <version>`
    pub created_by: String,
    /// `sha256`
    pub hash: String,
    pub piece_bytes: u64,
    /// reserved for client-side encryption; this build requires `null`
    #[serde(default)]
    pub encryption: Option<J>,
}

impl Marker {
    /// The marker of a new repository.
    pub fn new(id: Uuid, created: String) -> Marker {
        Marker {
            format: FORMAT,
            kind: MARKER_KIND.to_string(),
            id,
            created,
            created_by: format!("sparkles {}", env!("CARGO_PKG_VERSION")),
            hash: "sha256".to_string(),
            piece_bytes: PIECE_BYTES,
            encryption: None,
        }
    }

    /// Parse a stored marker and check that this build can use the repository
    /// (`422 incompatible-repository` otherwise).
    pub fn parse(bytes: &[u8]) -> Result<Marker> {
        let m: Marker = serde_json::from_slice(bytes).map_err(|e| {
            BackupError::new(
                Code::IncompatibleRepository,
                format!("{MARKER} does not parse: {e}"),
            )
        })?;
        m.check()?;
        Ok(m)
    }

    pub fn check(&self) -> Result<()> {
        let bad = |msg: String| Err(BackupError::new(Code::IncompatibleRepository, msg));
        if self.kind != MARKER_KIND {
            return bad(format!(
                "{MARKER}: kind {:?} is not {MARKER_KIND}",
                self.kind
            ));
        }
        if self.format > FORMAT {
            return bad(format!(
                "repository format {} is newer than this build reads ({FORMAT})",
                self.format
            ));
        }
        if self.hash != "sha256" {
            return bad(format!("unsupported blob hash {:?}", self.hash));
        }
        if self.encryption.as_ref().is_some_and(|e| !e.is_null()) {
            return bad("the repository is encrypted; this build cannot read it".into());
        }
        if self.piece_bytes == 0 || self.piece_bytes > 5 << 30 {
            return bad(format!("invalid pieceBytes {}", self.piece_bytes));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a marker serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let id = "ab".repeat(32);
        let k = blob_key(&id);
        assert_eq!(k.as_ref(), format!("blobs/ab/{id}"));
        assert_eq!(blob_id_of(&k), Some(id.as_str()));
        assert_eq!(blob_id_of(&Key::from(format!("blobs/cd/{id}"))), None);
        assert_eq!(blob_id_of(&Key::from("blobs/ab/xyz")), None);
        let m = manifest_key("b1.x");
        assert_eq!(m.as_ref(), "backups/b1.x.json");
        assert_eq!(backup_name_of(&m), Some("b1.x"));
        assert_eq!(backup_name_of(&Key::from("backups/.json")), None);
        assert_eq!(backup_name_of(&Key::from("backups/a/b.json")), None);
        assert_eq!(lock_id_of(&lock_key("x")), Some("x"));
        assert_eq!(
            probe_key(Uuid::nil()).as_ref(),
            format!("probe/{}", Uuid::nil())
        );
        assert_eq!(gc_last_key().as_ref(), "gc/last.json");
        assert_eq!(marker_key().as_ref(), "sparkles-repo.json");
    }

    #[test]
    fn grammars() {
        assert!(valid_repo_name("s3-main") && valid_repo_name("0_x"));
        for bad in ["", "S3", "-a", "a.b", &"a".repeat(65)] {
            assert!(!valid_repo_name(bad), "{bad}");
        }
        assert!(valid_backup_name("wiki-20260930t140311z") && valid_backup_name("B.1_x"));
        for bad in ["", ".a", "a/b", "a b", &"a".repeat(65)] {
            assert!(!valid_backup_name(bad), "{bad}");
        }
        assert!(valid_blob_id(&"0f".repeat(32)));
        assert!(!valid_blob_id(&"0F".repeat(32)) && !valid_blob_id("ABC"));
    }

    #[test]
    fn backup_paths() {
        for ok in [
            "CURRENT",
            "commits.bin",
            "validation-shapes.ttl",
            "validation-schema.shex",
            "validation-schema.json",
            "gen-0001/spo.dat",
            "gen-12345678/wal.log",
            "gen-0001/.hidden",
        ] {
            assert!(valid_backup_path(ok), "{ok}");
        }
        for bad in [
            "../x",
            "/etc/passwd",
            "gen-0001/../../x",
            "gen-0001/..",
            "gen-0001/.",
            "gen-001/spo.dat",
            "gen-0001/a/b",
            "gen-0001/",
            "sparkles.lock",
            "text/meta.json",
            "history.json",
            "CURRENT/x",
        ] {
            assert!(!valid_backup_path(bad), "{bad}");
        }
    }

    #[test]
    fn default_names_are_valid() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-09-30T14:03:11.482Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(time_tag(t), "20260930t140311z");
        assert_eq!(default_backup_name("wiki", t), "wiki-20260930t140311z");
        for ds in ["x".repeat(64), "_a".into(), "a b".into()] {
            let n = default_backup_name(&ds, t);
            assert!(valid_backup_name(&n), "{n}");
        }
    }

    #[test]
    fn marker_checks() {
        let m = Marker::new(Uuid::nil(), "2026-09-30T14:00:00.000Z".into());
        let back = Marker::parse(&m.to_bytes()).unwrap();
        assert_eq!(back, m);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["pieceBytes"], 33_554_432);
        assert!(v["encryption"].is_null());
        let with = |k: &str, val: J| {
            let mut v = v.clone();
            v[k] = val;
            Marker::parse(&serde_json::to_vec(&v).unwrap())
        };
        assert_eq!(
            with("format", 2.into()).unwrap_err().code(),
            Code::IncompatibleRepository
        );
        assert!(with("encryption", serde_json::json!({"scheme": "age"})).is_err());
        assert!(with("kind", "other".into()).is_err());
        assert!(with("unknownField", 1.into()).is_ok());
        assert!(Marker::parse(b"{").is_err());
    }
}
