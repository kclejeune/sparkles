//! The spatial index's files: a generation's base rows, tree and geometry column under
//! `gen-NNNN/geo/`, so that opening a store maps them instead of parsing every literal
//! again.
//!
//! Each file is a 64-byte header, sections aligned to 8 bytes, and a 32-byte footer
//! (little-endian throughout):
//!
//! ```text
//! header  0  magic  b"SPKGEO\0\x01"
//!         8  u32    file format version (FILE_VERSION)
//!        12  u32    kind (FileKind)
//!        16  u64    rows: tree items (rtree.spkg) or column entries (column.spkg)
//!        24  u64    GeoConfig::index_hash of the configuration it was built for
//!        32  u64    the generation's base commit (`seq`)
//!        40  u64    the generation's quad count
//!        48  u64    byte length of the index section (directory of the data)
//!        56  u32    CRC-32 of bytes 0..56
//!        60  u32    reserved (0)
//! footer  0  u64    rows (again)
//!         8  u32    CRC-32 of the index section
//!        12  u32    CRC-32 of the data sections (checked by `sparkles check`, not at open)
//!        16  u64    file length
//!        24  magic  b"SPKGEOF\x01"
//! ```
//!
//! A file is used only when magic, version, kind, configuration hash, base commit, quad
//! count, rows and both header and index checksums match; anything else is deleted and
//! the base is built again. Files are written to `*.tmp`, synced and renamed, and a
//! read-only store never writes them. A change of layout bumps [`FILE_VERSION`]; files
//! of other versions are rebuilt, never migrated.
//!
//! Not written or read yet: bases are built in memory.

/// The directory of a generation holding the index files.
pub const DIR: &str = "geo";
/// The base rows and their packed tree.
pub const RTREE_FILE: &str = "rtree.spkg";
/// The geometry column of the base literals.
pub const COLUMN_FILE: &str = "column.spkg";

pub const MAGIC: [u8; 8] = *b"SPKGEO\0\x01";
pub const FOOTER_MAGIC: [u8; 8] = *b"SPKGEOF\x01";
pub const FILE_VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 64;
pub const FOOTER_BYTES: usize = 32;

/// What a file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FileKind {
    Column = 1,
    Rtree = 2,
}

/// The identity of a file: what it must match to be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub kind: u32,
    pub rows: u64,
    pub config_hash: u64,
    pub base_seq: u64,
    pub quads: u64,
    pub index_bytes: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0u8; HEADER_BYTES];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..12].copy_from_slice(&self.version.to_le_bytes());
        b[12..16].copy_from_slice(&self.kind.to_le_bytes());
        b[16..24].copy_from_slice(&self.rows.to_le_bytes());
        b[24..32].copy_from_slice(&self.config_hash.to_le_bytes());
        b[32..40].copy_from_slice(&self.base_seq.to_le_bytes());
        b[40..48].copy_from_slice(&self.quads.to_le_bytes());
        b[48..56].copy_from_slice(&self.index_bytes.to_le_bytes());
        let crc = crc32(&[&b[0..56]]);
        b[56..60].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// The header of `b` (`None`: not a header of this format, or damaged).
    pub fn decode(b: &[u8]) -> Option<Header> {
        let b = b.get(..HEADER_BYTES)?;
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if b[0..8] != MAGIC || u32_at(56) != crc32(&[&b[0..56]]) {
            return None;
        }
        Some(Header {
            version: u32_at(8),
            kind: u32_at(12),
            rows: u64_at(16),
            config_hash: u64_at(24),
            base_seq: u64_at(32),
            quads: u64_at(40),
            index_bytes: u64_at(48),
        })
    }
}

/// CRC-32 (IEEE) of the concatenated parts.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = flate2::Crc::new();
    for p in parts {
        c.update(p);
    }
    c.sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = Header {
            version: FILE_VERSION,
            kind: FileKind::Rtree as u32,
            rows: 7,
            config_hash: 0xfeed,
            base_seq: 3,
            quads: 42,
            index_bytes: 4096,
        };
        let mut b = h.encode();
        assert_eq!(Header::decode(&b), Some(h));
        b[20] ^= 1;
        assert_eq!(Header::decode(&b), None);
    }
}
