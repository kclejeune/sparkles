//! Reading and validating manifests. A manifest comes from storage that someone else
//! may control: [`validate`] runs before anything is written for a restore.

use crate::error::Result;
use crate::layout::{MANIFEST_KIND, MAX_MANIFEST_BYTES};
use crate::{BackupError, Manifest};

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
/// * `dataset.id` and `commit` are well formed (`commit.generation == generation`,
///   `commit.ref == "commit:<seq>"`), `encryption` is null.
///
/// The content checks that need the downloaded files (`CURRENT` names `generation`;
/// `gen-NNNN/commit.json` has the dataset id and `baseSeq <= commit.seq`) are the
/// restore's.
pub fn validate(m: &Manifest, piece_bytes: u64, index_format: u32) -> Result<()> {
    let _ = (m, piece_bytes, index_format);
    Err(BackupError::unsupported("manifest validation"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Code;

    #[test]
    fn parse_refuses_what_it_cannot_read() {
        assert_eq!(parse(b"{}").unwrap_err().code(), Code::InvalidBackup);
        let big = vec![b' '; (MAX_MANIFEST_BYTES + 1) as usize];
        assert_eq!(parse(&big).unwrap_err().code(), Code::InvalidBackup);
    }
}
