//! Blob objects (`blobs/<hh>/<id>`): the content of one piece or segment of a file.
//!
//! A blob's id is the lowercase hex SHA-256 of its plaintext, so ids do not depend on
//! the encoding and two writers that compress differently still deduplicate. The stored
//! object is a 16-byte little-endian header followed by the payload:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0..4 | magic `SPKB` |
//! | 4 | format `1` |
//! | 5 | codec: `0` raw, `1` LZ4 frame, `2` zstd (reserved) |
//! | 6 | encryption: `0` none |
//! | 7 | zero |
//! | 8..16 | plaintext length |
//! | 16.. | payload |
//!
//! Readers refuse unknown formats, codecs and encryption values, then check the decoded
//! length and the SHA-256 against the id.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::io::Read;

pub const MAGIC: &[u8; 4] = b"SPKB";
pub const FORMAT: u8 = 1;
pub const HEADER_LEN: usize = 16;

/// How a blob's payload is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Raw = 0,
    /// an LZ4 frame (`lz4_flex`)
    Lz4 = 1,
    /// reserved: this build neither writes nor reads it
    Zstd = 2,
}

impl Codec {
    fn from_byte(b: u8) -> Option<Codec> {
        match b {
            0 => Some(Codec::Raw),
            1 => Some(Codec::Lz4),
            2 => Some(Codec::Zstd),
            _ => None,
        }
    }
}

/// Why a stored blob could not be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    /// not a blob object, or a damaged header or payload
    #[error("malformed blob {id}: {reason}")]
    Malformed { id: String, reason: String },
    /// a blob written by a newer or differently configured writer
    #[error("unsupported blob {id}: {reason}")]
    Unsupported { id: String, reason: String },
    /// the decoded content does not have the expected length or hash
    #[error("checksum mismatch in blob {id}")]
    Mismatch { id: String },
}

impl From<BlobError> for crate::BackupError {
    fn from(e: BlobError) -> crate::BackupError {
        crate::BackupError::new(crate::Code::Internal, e.to_string())
    }
}

/// Lowercase hex SHA-256 of `data`: a blob id.
pub fn blob_id(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Lowercase hex of bytes.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

/// A streaming SHA-256 whose result is a hex string (whole-file hashes).
#[derive(Clone, Default)]
pub struct Hasher(Sha256);

impl Hasher {
    pub fn new() -> Hasher {
        Hasher(Sha256::new())
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finish(self) -> String {
        hex(&self.0.finalize())
    }
}

/// The 16-byte header of a blob.
pub fn header(codec: Codec, plain_len: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(MAGIC);
    h[4] = FORMAT;
    h[5] = codec as u8;
    h[8..16].copy_from_slice(&plain_len.to_le_bytes());
    h
}

/// A plaintext encoded for storage.
#[derive(Clone, Debug)]
pub struct Encoded {
    /// header and payload: the object to `PUT`
    pub bytes: Bytes,
    pub codec: Codec,
    /// the id of the plaintext
    pub id: String,
}

/// Encode `plain` as a blob object. With `compress`, the payload is an LZ4 frame when
/// that saves at least 10 %, else it is stored raw.
pub fn encode(plain: &[u8], compress: bool) -> Encoded {
    encode_with_id(plain, blob_id(plain), compress)
}

/// [`encode`] for a plaintext whose id is already known.
pub fn encode_with_id(plain: &[u8], id: String, compress: bool) -> Encoded {
    if compress && !plain.is_empty() {
        use std::io::Write;
        let mut out = header(Codec::Lz4, plain.len() as u64).to_vec();
        let mut enc = lz4_flex::frame::FrameEncoder::new(Vec::with_capacity(plain.len() / 2));
        // writing into a Vec cannot fail
        if enc.write_all(plain).is_ok()
            && let Ok(frame) = enc.finish()
            && frame.len() * 10 <= plain.len() * 9
        {
            out.extend_from_slice(&frame);
            return Encoded {
                bytes: Bytes::from(out),
                codec: Codec::Lz4,
                id,
            };
        }
    }
    let mut out = Vec::with_capacity(HEADER_LEN + plain.len());
    out.extend_from_slice(&header(Codec::Raw, plain.len() as u64));
    out.extend_from_slice(plain);
    Encoded {
        bytes: Bytes::from(out),
        codec: Codec::Raw,
        id,
    }
}

/// The parsed header of a stored blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub codec: Codec,
    pub plain_len: u64,
}

/// Parse and check the header of the stored blob `id`.
pub fn parse_header(stored: &[u8], id: &str) -> Result<Header, BlobError> {
    let malformed = |reason: &str| BlobError::Malformed {
        id: id.to_string(),
        reason: reason.to_string(),
    };
    let unsupported = |reason: String| BlobError::Unsupported {
        id: id.to_string(),
        reason,
    };
    if stored.len() < HEADER_LEN {
        return Err(malformed("shorter than its header"));
    }
    if &stored[..4] != MAGIC {
        return Err(malformed("bad magic"));
    }
    if stored[4] != FORMAT {
        return Err(unsupported(format!("format {}", stored[4])));
    }
    let codec = match Codec::from_byte(stored[5]) {
        Some(c @ (Codec::Raw | Codec::Lz4)) => c,
        _ => return Err(unsupported(format!("codec {}", stored[5]))),
    };
    if stored[6] != 0 {
        return Err(unsupported(format!("encryption {}", stored[6])));
    }
    if stored[7] != 0 {
        return Err(malformed("reserved header byte is not zero"));
    }
    let plain_len = u64::from_le_bytes(stored[8..16].try_into().unwrap());
    Ok(Header { codec, plain_len })
}

/// Decode the stored blob `id` and check it: the header, the plaintext length
/// (`expect_len`, from the manifest) and the SHA-256. Never decodes more than
/// `expect_len` bytes, whatever the header claims.
pub fn decode(stored: &[u8], id: &str, expect_len: u64) -> Result<Vec<u8>, BlobError> {
    let h = parse_header(stored, id)?;
    let mismatch = || BlobError::Mismatch { id: id.to_string() };
    if h.plain_len != expect_len {
        return Err(mismatch());
    }
    let payload = &stored[HEADER_LEN..];
    let plain = match h.codec {
        Codec::Raw => payload.to_vec(),
        Codec::Lz4 => {
            let mut out = Vec::with_capacity(expect_len.min(64 << 20) as usize);
            lz4_flex::frame::FrameDecoder::new(payload)
                .take(expect_len + 1)
                .read_to_end(&mut out)
                .map_err(|e| BlobError::Malformed {
                    id: id.to_string(),
                    reason: format!("LZ4 frame: {e}"),
                })?;
            out
        }
        Codec::Zstd => unreachable!("refused by parse_header"),
    };
    if plain.len() as u64 != expect_len || blob_id(&plain) != id {
        return Err(mismatch());
    }
    Ok(plain)
}

/// The pieces of a file of `len` bytes cut at every `piece` bytes: `(offset, length)`.
/// An empty file has one empty piece (a blob of zero bytes), so every file has a blob.
pub fn pieces(len: u64, piece: u64) -> impl Iterator<Item = (u64, u64)> {
    assert!(piece > 0);
    let n = len.div_ceil(piece).max(1);
    (0..n).map(move |i| {
        let off = i * piece;
        (off, (len - off.min(len)).min(piece))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_sha256_hex() {
        assert_eq!(
            blob_id(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            blob_id(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut h = Hasher::new();
        h.update(b"a");
        h.update(b"bc");
        assert_eq!(h.finish(), blob_id(b"abc"));
    }

    #[test]
    fn raw_and_lz4_round_trip() {
        let text = "prefix ex: <http://example.org/> ".repeat(1000);
        let e = encode(text.as_bytes(), true);
        assert_eq!(e.codec, Codec::Lz4);
        assert!(e.bytes.len() < text.len() / 2);
        assert_eq!(&e.bytes[..4], MAGIC);
        assert_eq!(
            decode(&e.bytes, &e.id, text.len() as u64).unwrap(),
            text.as_bytes()
        );

        // incompressible data stays raw
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                // xorshift64
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect();
        let e = encode(&noise, true);
        assert_eq!(e.codec, Codec::Raw);
        assert_eq!(e.bytes.len(), HEADER_LEN + noise.len());
        assert_eq!(decode(&e.bytes, &e.id, noise.len() as u64).unwrap(), noise);

        let e = encode(b"", true);
        assert_eq!(e.codec, Codec::Raw);
        assert_eq!(decode(&e.bytes, &e.id, 0).unwrap(), b"");
        // ids do not depend on the encoding
        assert_eq!(
            encode(text.as_bytes(), false).id,
            encode(text.as_bytes(), true).id
        );
    }

    #[test]
    fn damage_is_detected() {
        let text = "x".repeat(10_000);
        for compress in [false, true] {
            let e = encode(text.as_bytes(), compress);
            let len = text.len() as u64;
            // a flipped payload byte
            let mut b = e.bytes.to_vec();
            let last = b.len() - 1;
            b[last] ^= 1;
            assert!(decode(&b, &e.id, len).is_err());
            // wrong expected length or id
            assert_eq!(
                decode(&e.bytes, &e.id, len - 1),
                Err(BlobError::Mismatch { id: e.id.clone() })
            );
            assert!(matches!(
                decode(&e.bytes, &blob_id(b"other"), len),
                Err(BlobError::Mismatch { .. })
            ));
        }
        let e = encode(b"hello", false);
        let mut b = e.bytes.to_vec();
        b[0] = b'X';
        assert!(matches!(
            decode(&b, &e.id, 5),
            Err(BlobError::Malformed { .. })
        ));
        for (i, v) in [(4, 2u8), (5, 2), (5, 9), (6, 1)] {
            let mut b = e.bytes.to_vec();
            b[i] = v;
            assert!(
                matches!(decode(&b, &e.id, 5), Err(BlobError::Unsupported { .. })),
                "byte {i} = {v}"
            );
        }
        assert!(decode(&e.bytes[..10], &e.id, 5).is_err());
    }

    #[test]
    fn lz4_output_is_capped() {
        // a header that understates the plaintext cannot make decode read it all
        let big = vec![0u8; 1 << 20];
        let e = encode(&big, true);
        assert_eq!(e.codec, Codec::Lz4);
        let mut b = e.bytes.to_vec();
        b[8..16].copy_from_slice(&10u64.to_le_bytes());
        assert!(matches!(
            decode(&b, &e.id, 10),
            Err(BlobError::Mismatch { .. })
        ));
    }

    #[test]
    fn piece_boundaries() {
        let p = |len, piece| pieces(len, piece).collect::<Vec<_>>();
        assert_eq!(p(0, 4), vec![(0, 0)]);
        assert_eq!(p(3, 4), vec![(0, 3)]);
        assert_eq!(p(4, 4), vec![(0, 4)]);
        assert_eq!(p(9, 4), vec![(0, 4), (4, 4), (8, 1)]);
    }
}
