use super::primitive;
use crate::{BackupError, Result, blob, layout};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_PIECE_BYTES: u64 = 64 << 20;
pub const MAX_STORED_MANIFEST_BYTES: u64 = 24 << 20;

/// One epoch's keys; deliberately no Serialize or key-bearing Debug.
pub(crate) struct EpochKeys {
    pub repository: Uuid,
    pub epoch: u32,
    pub master: super::memory::LockedKey,
    pub id: super::memory::LockedKey,
    blob: super::memory::LockedKey,
    manifest: super::memory::LockedKey,
}

impl std::fmt::Debug for EpochKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochKeys")
            .field("repository", &self.repository)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl EpochKeys {
    pub fn new(repository: Uuid, epoch: u32, master: Zeroizing<[u8; 32]>) -> Result<Self> {
        if repository.is_nil() || epoch == 0 {
            return Err(invalid("invalid repository identity or epoch"));
        }
        Ok(Self {
            repository,
            epoch,
            id: super::memory::LockedKey::new(primitive::derive(
                master.as_ref(),
                &[],
                b"sparkles/f07/repo/id",
            )?)?,
            blob: super::memory::LockedKey::new(primitive::derive(
                master.as_ref(),
                &[],
                b"sparkles/f07/repo/blob",
            )?)?,
            manifest: super::memory::LockedKey::new(primitive::derive(
                master.as_ref(),
                &[],
                b"sparkles/f07/repo/manifest",
            )?)?,
            master: super::memory::LockedKey::new(master)?,
        })
    }

    pub fn blob_id(&self, plain: &[u8]) -> String {
        blob::hex(&primitive::mac(self.id.as_ref(), plain))
    }

    pub fn seal_blob(&self, plain: &[u8], compress: bool) -> Result<blob::Encoded> {
        if plain.len() as u64 > MAX_PIECE_BYTES {
            return Err(invalid("piece exceeds encryption limit"));
        }
        let encoded = blob::encode(plain, compress);
        let id = self.blob_id(plain);
        let mut inner = Zeroizing::new(Vec::with_capacity(encoded.bytes.len()));
        inner.push(encoded.codec as u8);
        inner.extend_from_slice(&(plain.len() as u64).to_le_bytes());
        inner.extend_from_slice(&encoded.bytes[blob::HEADER_LEN..]);
        let padded = primitive::padme(inner.len() as u64)? as usize;
        inner.resize(padded, 0);
        let salt = primitive::random::<16>()?;
        let mut header = [0u8; 32];
        header[..4].copy_from_slice(blob::MAGIC);
        header[4] = blob::FORMAT;
        header[6] = 1;
        header[8..12].copy_from_slice(&self.epoch.to_le_bytes());
        header[16..32].copy_from_slice(&salt);
        let mut aad = header.to_vec();
        aad.extend_from_slice(self.repository.as_bytes());
        aad.extend_from_slice(&decode_id(&id)?);
        let key = primitive::derive(self.blob.as_ref(), &salt, b"sparkles/f07/repo/blob")?;
        let ct = primitive::seal(key.as_ref(), [0; 12], &aad, &inner)?;
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&ct);
        Ok(blob::Encoded {
            bytes: bytes.into(),
            codec: encoded.codec,
            id,
        })
    }

    pub fn open_blob(&self, stored: &[u8], id: &str, expected: u64) -> Result<Vec<u8>> {
        self.open_blob_inner(stored, id, Some(expected))
    }
    pub fn check_orphan(&self, stored: &[u8], id: &str) -> Result<()> {
        self.open_blob_inner(stored, id, None).map(|_| ())
    }
    fn open_blob_inner(&self, stored: &[u8], id: &str, expected: Option<u64>) -> Result<Vec<u8>> {
        let bound = expected.unwrap_or(MAX_PIECE_BYTES);
        if bound > MAX_PIECE_BYTES
            || stored.len() as u64 > stored_blob_limit(bound)?
            || stored.len() < 48
        {
            return Err(invalid("invalid encrypted blob size"));
        }
        let header = &stored[..32];
        if header[..4] != *blob::MAGIC
            || header[4] != blob::FORMAT
            || header[5] != 0
            || header[6] != 1
            || header[7] != 0
            || header[12..16] != [0; 4]
            || u32::from_le_bytes(header[8..12].try_into().expect("header checked")) != self.epoch
        {
            return Err(invalid("invalid encrypted blob header"));
        }
        let mut aad = header.to_vec();
        aad.extend_from_slice(self.repository.as_bytes());
        aad.extend_from_slice(&decode_id(id)?);
        let key = primitive::derive(
            self.blob.as_ref(),
            &header[16..32],
            b"sparkles/f07/repo/blob",
        )?;
        let inner = primitive::open(key.as_ref(), [0; 12], &aad, &stored[32..])?;
        if inner.len() < 9 {
            return Err(invalid("invalid encrypted plaintext length"));
        }
        let actual = u64::from_le_bytes(inner[1..9].try_into().expect("inner checked"));
        if actual > MAX_PIECE_BYTES || expected.is_some_and(|expected| expected != actual) {
            return Err(invalid("invalid encrypted plaintext length"));
        }
        let expected = actual;
        let (plain, consumed) = match inner[0] {
            0 => {
                let end = 9usize
                    .checked_add(expected as usize)
                    .ok_or_else(|| invalid("invalid plaintext length"))?;
                if end > inner.len() {
                    return Err(invalid("truncated encrypted blob"));
                }
                (inner[9..end].to_vec(), end)
            }
            1 => {
                let mut cursor = Cursor::new(&inner[9..]);
                let mut plain = Vec::with_capacity(expected as usize);
                lz4_flex::frame::FrameDecoder::new(&mut cursor)
                    .take(expected + 1)
                    .read_to_end(&mut plain)
                    .map_err(|_| invalid("invalid encrypted compressed payload"))?;
                (plain, 9 + cursor.position() as usize)
            }
            _ => return Err(invalid("unsupported encrypted codec")),
        };
        if plain.len() as u64 != expected
            || self.blob_id(&plain) != id
            || consumed > inner.len()
            || primitive::padme(consumed as u64)? != inner.len() as u64
            || inner[consumed..].iter().any(|byte| *byte != 0)
        {
            return Err(invalid("invalid encrypted content or padding"));
        }
        Ok(plain)
    }

    pub fn seal_manifest(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        if plain.len() as u64 > layout::MAX_MANIFEST_BYTES || !layout::valid_backup_name(name) {
            return Err(invalid("invalid manifest size or name"));
        }
        let salt = primitive::random::<16>()?;
        let key = primitive::derive(self.manifest.as_ref(), &salt, b"sparkles/f07/repo/manifest")?;
        let aad = manifest_aad(self.repository, self.epoch, name);
        let ct = primitive::seal(key.as_ref(), [0; 12], &aad, plain)?;
        serde_json::to_vec(&Envelope {
            format: 1,
            kind: "sparkles-backup-sealed".into(),
            name: name.into(),
            repository_id: self.repository,
            epoch: self.epoch,
            salt: STANDARD.encode(salt),
            ct: STANDARD.encode(ct),
        })
        .map_err(|_| invalid("cannot encode encrypted manifest"))
    }

    pub fn open_manifest(&self, name: &str, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let envelope = Envelope::parse(bytes)?;
        if envelope.repository_id != self.repository
            || envelope.epoch != self.epoch
            || envelope.name != name
        {
            return Err(invalid(
                "encrypted manifest identity does not match its repository or name",
            ));
        }
        let salt = decode_bounded(&envelope.salt, 16)?;
        if salt.len() != 16 {
            return Err(invalid("invalid manifest salt"));
        }
        let ct = decode_bounded(
            &envelope.ct,
            layout::MAX_MANIFEST_BYTES as usize + primitive::TAG_BYTES,
        )?;
        let key = primitive::derive(self.manifest.as_ref(), &salt, b"sparkles/f07/repo/manifest")?;
        let plain = primitive::open(
            key.as_ref(),
            [0; 12],
            &manifest_aad(self.repository, self.epoch, name),
            &ct,
        )?;
        if plain.len() as u64 > layout::MAX_MANIFEST_BYTES {
            return Err(invalid("encrypted manifest plaintext exceeds limit"));
        }
        Ok(plain)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Envelope {
    pub format: u32,
    pub kind: String,
    pub name: String,
    pub repository_id: Uuid,
    pub epoch: u32,
    pub salt: String,
    pub ct: String,
}
impl Envelope {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_STORED_MANIFEST_BYTES {
            return Err(invalid("encrypted manifest envelope exceeds limit"));
        }
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|_| invalid("invalid encrypted manifest envelope"))?;
        if value.format != 1
            || value.kind != "sparkles-backup-sealed"
            || value.repository_id.is_nil()
            || value.epoch == 0
            || !layout::valid_backup_name(&value.name)
        {
            return Err(invalid("unsupported encrypted manifest envelope"));
        }
        Ok(value)
    }
}

pub fn stored_blob_limit(plain: u64) -> Result<u64> {
    if plain > MAX_PIECE_BYTES {
        return Err(invalid("piece exceeds encryption limit"));
    }
    primitive::padme(plain + 9)?
        .checked_add(48)
        .ok_or_else(|| invalid("encrypted blob size overflow"))
}
pub(crate) fn invalid(message: &str) -> BackupError {
    BackupError::invalid_backup("encryption", message)
}
pub(crate) fn decode_bounded(value: &str, limit: usize) -> Result<Vec<u8>> {
    if value.len() > limit.saturating_add(2) / 3 * 4 {
        return Err(invalid("encoded value exceeds limit"));
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| invalid("invalid encoded value"))?;
    if bytes.len() > limit {
        return Err(invalid("decoded value exceeds limit"));
    }
    Ok(bytes)
}
fn decode_id(value: &str) -> Result<[u8; 32]> {
    if !layout::valid_blob_id(value) {
        return Err(invalid("invalid keyed blob identity"));
    }
    let mut out = [0; 32];
    for (i, bytes) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        out[i] = digit(bytes[0]) * 16 + digit(bytes[1]);
    }
    Ok(out)
}
fn manifest_aad(repo: Uuid, epoch: u32, name: &str) -> Vec<u8> {
    let mut bytes = repo.as_bytes().to_vec();
    bytes.extend_from_slice(&epoch.to_le_bytes());
    bytes.extend_from_slice(name.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_invalid_lengths_codecs_padding_and_extra_frames_are_refused() {
        let keys = EpochKeys::new(Uuid::new_v4(), 1, Zeroizing::new([49; 32])).unwrap();
        let plain = [1u8; 100];
        let valid = keys.seal_blob(&plain, false).unwrap();
        let seal_inner = |inner: &[u8]| {
            let header = &valid.bytes[..32];
            let mut aad = header.to_vec();
            aad.extend_from_slice(keys.repository.as_bytes());
            aad.extend_from_slice(&decode_id(&valid.id).unwrap());
            let key = primitive::derive(
                keys.blob.as_ref(),
                &header[16..32],
                b"sparkles/f07/repo/blob",
            )
            .unwrap();
            let mut bytes = header.to_vec();
            bytes.extend_from_slice(&primitive::seal(key.as_ref(), [0; 12], &aad, inner).unwrap());
            bytes
        };
        let mut inner = vec![0];
        inner.extend_from_slice(&100u64.to_le_bytes());
        inner.extend_from_slice(&plain);
        inner.resize(primitive::padme(inner.len() as u64).unwrap() as usize, 0);
        for offset in [0, 1, inner.len() - 1] {
            let mut bad = inner.clone();
            bad[offset] ^= 1;
            assert!(keys.open_blob(&seal_inner(&bad), &valid.id, 100).is_err());
        }
        let encoded = blob::encode(&plain, true);
        let mut two_frames = vec![1];
        two_frames.extend_from_slice(&100u64.to_le_bytes());
        two_frames.extend_from_slice(&encoded.bytes[blob::HEADER_LEN..]);
        two_frames.extend_from_slice(&encoded.bytes[blob::HEADER_LEN..]);
        two_frames.resize(
            primitive::padme(two_frames.len() as u64).unwrap() as usize,
            0,
        );
        assert!(
            keys.open_blob(&seal_inner(&two_frames), &valid.id, 100)
                .is_err()
        );
        for offset in [0, 4, 5, 6, 7, 8, 12, 16, 31, 32, valid.bytes.len() - 1] {
            let mut bad = valid.bytes.to_vec();
            bad[offset] ^= 1;
            assert!(keys.open_blob(&bad, &valid.id, 100).is_err());
        }
        assert!(
            keys.open_blob(&valid.bytes, &valid.id, MAX_PIECE_BYTES + 1)
                .is_err()
        );
    }

    #[test]
    fn blobs_and_manifests_authenticate_repository_name_id_and_header() {
        let a = EpochKeys::new(Uuid::new_v4(), 1, Zeroizing::new([7; 32])).unwrap();
        let b = EpochKeys::new(Uuid::new_v4(), 1, Zeroizing::new([8; 32])).unwrap();
        for plain in [vec![], vec![0; 12000], b"uncompressible literal".to_vec()] {
            for compress in [false, true] {
                let x = a.seal_blob(&plain, compress).unwrap();
                let y = a.seal_blob(&plain, compress).unwrap();
                assert_eq!(x.id, y.id);
                assert_ne!(x.bytes, y.bytes);
                assert_ne!(a.blob_id(&plain), b.blob_id(&plain));
                assert_eq!(
                    a.open_blob(&x.bytes, &x.id, plain.len() as u64).unwrap(),
                    plain
                );
                assert!(b.open_blob(&x.bytes, &x.id, plain.len() as u64).is_err());
                assert!(
                    a.open_blob(&x.bytes, &"0".repeat(64), plain.len() as u64)
                        .is_err()
                );
                let mut bad = x.bytes.to_vec();
                bad[16] ^= 1;
                assert!(a.open_blob(&bad, &x.id, plain.len() as u64).is_err());
            }
        }
        let sealed = a.seal_manifest("backup", b"{\"secret\":123}").unwrap();
        assert_eq!(
            a.open_manifest("backup", &sealed).unwrap().as_slice(),
            b"{\"secret\":123}"
        );
        assert!(a.open_manifest("renamed", &sealed).is_err());
        assert!(b.open_manifest("backup", &sealed).is_err());
        assert!(
            !a.open_manifest("backup", b"{\"ct\":\"DO_NOT_PRINT_THIS\"}")
                .unwrap_err()
                .to_string()
                .contains("DO_NOT_PRINT_THIS")
        );
    }
}
