//! The files of a configured vector index: one file per index and generation,
//! `gen-NNNN/vectors/<name>.spkv`, holding the packed base vectors and the HNSW graph, so
//! that opening a store maps them instead of parsing every literal and building the graph
//! again.
//!
//! The layout is little-endian, every section starting at a multiple of 64 bytes:
//!
//! ```text
//! header   0  magic  b"SPKVEC\0\x01"
//!          8  u32    file format version (FILE_VERSION)
//!         12  u32    sections
//!         16  u64    rows
//!         24  u64    VectorIndexConfig::build_hash of the configuration it was built for
//!         32  u64    the generation's base commit (`seq`)
//!         40  u64    the generation's quad count
//!         48  u64    the generation's term count
//!         56  u32    CRC-32 of bytes 0..56
//!         60  u32    reserved (0)
//! table       (u64 offset, u64 length) per section
//! sections    meta (u64 words), ids ([s, o, g] u64 per row), norms (f32 per row),
//!             data (f32, dim per row), then for an index with a graph: the graph's node
//!             rows (u32), layer 0 (u32), and per upper layer its nodes and links (u32)
//! footer   0  u32    CRC-32 of the header, table, meta and ids
//!          4  u32    CRC-32 of the other sections
//!          8  u64    file length
//!         16  u64    rows (again)
//!         24  magic  b"SPKVECF\x01"
//! ```
//!
//! A file is used only when the magic, version, configuration hash, base commit, quad and
//! term counts, row count, file length and the first checksum match; the second one is
//! verified by `sparkles check --full`. Anything else is deleted and the index is built
//! again. Files are written to `*.tmp`, synced and renamed, never changed in place, so a
//! mapping stays valid whatever later builds or compactions do.

use crate::error::Result;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const DIR: &str = "vectors";
pub const EXT: &str = "spkv";
pub const MAGIC: [u8; 8] = *b"SPKVEC\0\x01";
pub const FOOTER_MAGIC: [u8; 8] = *b"SPKVECF\x01";
pub const FILE_VERSION: u32 = 1;
const HEADER: usize = 64;
const FOOTER: usize = 32;
const ALIGN: usize = 64;

/// Files are read in place only on little-endian targets.
pub(crate) const SUPPORTED: bool = cfg!(target_endian = "little");

/// Plain values a section can hold.
pub trait Pod: Copy + Send + Sync + 'static {}
impl Pod for u32 {}
impl Pod for u64 {}
impl Pod for f32 {}
impl Pod for [u64; 3] {}

/// A slice of values in memory or in a mapped file.
pub enum Slice<T: Pod> {
    Owned(Vec<T>),
    Mapped {
        map: Arc<memmap2::Mmap>,
        off: usize,
        len: usize,
    },
}

impl<T: Pod> Default for Slice<T> {
    fn default() -> Self {
        Slice::Owned(Vec::new())
    }
}

impl<T: Pod> std::ops::Deref for Slice<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Slice::Owned(v) => v,
            // SAFETY: `Mapped` slices are made only by `Mapped::slice`, which checks the
            // bounds and the alignment; the mapping lives as long as `map` and its file
            // is never changed in place.
            Slice::Mapped { map, off, len } => unsafe {
                std::slice::from_raw_parts(map.as_ptr().add(*off).cast::<T>(), *len)
            },
        }
    }
}

impl<T: Pod> Slice<T> {
    pub fn is_mapped(&self) -> bool {
        matches!(self, Slice::Mapped { .. })
    }

    /// Bytes held in memory (0 when mapped).
    pub fn heap_bytes(&self) -> u64 {
        match self {
            Slice::Owned(v) => std::mem::size_of_val(v.as_slice()) as u64,
            Slice::Mapped { .. } => 0,
        }
    }
}

fn bytes_of<T: Pod>(v: &[T]) -> &[u8] {
    // SAFETY: `Pod` types are plain numbers (or arrays of them) without padding.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

/// CRC-32 (IEEE) of the concatenated parts.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = flate2::Crc::new();
    for p in parts {
        c.update(p);
    }
    c.sum()
}

/// What a file must match to be used: the configuration's build hash and the
/// generation's base commit, quad and term counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub config_hash: u64,
    pub base_seq: u64,
    pub quads: u64,
    pub terms: u64,
}

impl Identity {
    /// The identity of the generation in `dir` (base commit from its `commit.json`, 0
    /// without one).
    pub fn of(dir: &Path, quads: u64, terms: u64, config_hash: u64) -> Identity {
        let base_seq = crate::commit::read_gen_commit(dir)
            .ok()
            .flatten()
            .map_or(0, |(_, c, _)| c.seq);
        Identity {
            config_hash,
            base_seq,
            quads,
            terms,
        }
    }
}

/// The directory of a generation's vector index files.
pub fn dir_of(gen_dir: &Path) -> PathBuf {
    gen_dir.join(DIR)
}

/// The file of index `name` in `dir`.
pub fn file_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.{EXT}"))
}

/// Why a file cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Missing,
    Unusable(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Missing => f.write_str("missing"),
            Problem::Unusable(m) => f.write_str(m),
        }
    }
}

/// A section to write: its bytes.
pub struct Out<'a>(pub &'a [u8]);

impl<'a> Out<'a> {
    pub fn of<T: Pod>(v: &'a [T]) -> Out<'a> {
        Out(bytes_of(v))
    }
}

fn pad(n: usize) -> usize {
    n.div_ceil(ALIGN) * ALIGN
}

/// Write `path` durably (a synced temporary file renamed over it, then the directory
/// synced). The first two sections after `meta` (ids) are covered by the open checksum.
pub fn write(path: &Path, ident: &Identity, rows: u64, sections: &[Out<'_>]) -> Result<()> {
    let n = sections.len();
    let table_end = HEADER + n * 16;
    let mut offsets = Vec::with_capacity(n);
    let mut at = pad(table_end);
    for s in sections {
        offsets.push((at as u64, s.0.len() as u64));
        at = pad(at + s.0.len());
    }
    let len = at + FOOTER;
    let mut header = [0u8; HEADER];
    header[0..8].copy_from_slice(&MAGIC);
    header[8..12].copy_from_slice(&FILE_VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&(n as u32).to_le_bytes());
    header[16..24].copy_from_slice(&rows.to_le_bytes());
    header[24..32].copy_from_slice(&ident.config_hash.to_le_bytes());
    header[32..40].copy_from_slice(&ident.base_seq.to_le_bytes());
    header[40..48].copy_from_slice(&ident.quads.to_le_bytes());
    header[48..56].copy_from_slice(&ident.terms.to_le_bytes());
    let hcrc = crc32(&[&header[0..56]]);
    header[56..60].copy_from_slice(&hcrc.to_le_bytes());
    let mut table = Vec::with_capacity(n * 16);
    for (o, l) in &offsets {
        table.extend_from_slice(&o.to_le_bytes());
        table.extend_from_slice(&l.to_le_bytes());
    }
    let head_crc = {
        let mut parts: Vec<&[u8]> = vec![&header, &table];
        parts.extend(sections.iter().take(2).map(|s| s.0));
        crc32(&parts)
    };
    let mut c = flate2::Crc::new();
    for s in sections.iter().skip(2) {
        c.update(s.0);
    }
    let rest_crc = c.sum();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("{EXT}.tmp"));
    let r = (|| -> Result<()> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        w.write_all(&header)?;
        w.write_all(&table)?;
        let mut pos = table_end;
        let zeros = [0u8; ALIGN];
        for (s, (o, _)) in sections.iter().zip(&offsets) {
            w.write_all(&zeros[..*o as usize - pos])?;
            w.write_all(s.0)?;
            pos = *o as usize + s.0.len();
        }
        w.write_all(&zeros[..pad(pos) - pos])?;
        let mut footer = [0u8; FOOTER];
        footer[0..4].copy_from_slice(&head_crc.to_le_bytes());
        footer[4..8].copy_from_slice(&rest_crc.to_le_bytes());
        footer[8..16].copy_from_slice(&(len as u64).to_le_bytes());
        footer[16..24].copy_from_slice(&rows.to_le_bytes());
        footer[24..32].copy_from_slice(&FOOTER_MAGIC);
        w.write_all(&footer)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)?;
    crate::store::sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// A mapped and checked file.
pub struct Mapped {
    map: Arc<memmap2::Mmap>,
    pub rows: u64,
    table: Vec<(usize, usize)>,
}

impl Mapped {
    /// Map and check `path`; with `ident`, it must belong to that generation and
    /// configuration; with `verify_data`, every section's checksum is verified.
    pub fn open(
        path: &Path,
        ident: Option<&Identity>,
        verify_data: bool,
    ) -> std::result::Result<Mapped, Problem> {
        let bad = |m: &str| Problem::Unusable(m.to_string());
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Problem::Missing),
            Err(e) => return Err(Problem::Unusable(format!("cannot be read: {e}"))),
        };
        // SAFETY: files are written once (to a temporary name, then renamed) and never
        // changed in place.
        let map = unsafe { memmap2::Mmap::map(&f) }
            .map_err(|e| Problem::Unusable(format!("cannot be mapped: {e}")))?;
        let n = map.len();
        if n < HEADER + FOOTER || !SUPPORTED {
            return Err(bad("truncated"));
        }
        let u32_at = |i: usize| u32::from_le_bytes(map[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(map[i..i + 8].try_into().unwrap());
        if map[0..8] != MAGIC || u32_at(56) != crc32(&[&map[0..56]]) {
            return Err(bad("damaged header"));
        }
        if u32_at(8) != FILE_VERSION {
            return Err(Problem::Unusable(format!(
                "format version {} (this build reads {FILE_VERSION})",
                u32_at(8)
            )));
        }
        let f0 = n - FOOTER;
        if map[f0 + 24..n] != FOOTER_MAGIC {
            return Err(bad("damaged or missing footer (truncated?)"));
        }
        if u64_at(f0 + 8) != n as u64 {
            return Err(bad("truncated or extended"));
        }
        let rows = u64_at(16);
        if u64_at(f0 + 16) != rows {
            return Err(bad("header and footer disagree"));
        }
        let sections = u32_at(12) as usize;
        let table_end = HEADER + sections * 16;
        if table_end > f0 {
            return Err(bad("section table out of bounds"));
        }
        let mut table = Vec::with_capacity(sections);
        for i in 0..sections {
            let (o, l) = (
                u64_at(HEADER + i * 16) as usize,
                u64_at(HEADER + i * 16 + 8) as usize,
            );
            if !o.is_multiple_of(ALIGN) || o < table_end || o.checked_add(l).is_none_or(|e| e > f0)
            {
                return Err(bad("section out of bounds"));
            }
            table.push((o, l));
        }
        let mut parts: Vec<&[u8]> = vec![&map[0..table_end]];
        parts.extend(table.iter().take(2).map(|&(o, l)| &map[o..o + l]));
        if crc32(&parts) != u32_at(f0) {
            return Err(bad("checksum mismatch"));
        }
        if verify_data {
            let rest: Vec<&[u8]> = table.iter().skip(2).map(|&(o, l)| &map[o..o + l]).collect();
            if crc32(&rest) != u32_at(f0 + 4) {
                return Err(bad("data checksum mismatch"));
            }
        }
        if let Some(id) = ident {
            if u64_at(24) != id.config_hash {
                return Err(bad("built for another configuration"));
            }
            if (u64_at(32), u64_at(40), u64_at(48)) != (id.base_seq, id.quads, id.terms) {
                return Err(bad("built for another generation"));
            }
        }
        Ok(Mapped {
            map: Arc::new(map),
            rows,
            table,
        })
    }

    pub fn sections(&self) -> usize {
        self.table.len()
    }

    /// Bytes of the file.
    pub fn file_len(&self) -> u64 {
        self.map.len() as u64
    }

    /// Section `i` as values of `T` (`None`: out of range or misaligned).
    pub fn slice<T: Pod>(&self, i: usize) -> Option<Slice<T>> {
        let &(off, len) = self.table.get(i)?;
        let size = std::mem::size_of::<T>();
        if !len.is_multiple_of(size)
            || !(self.map.as_ptr() as usize + off).is_multiple_of(std::mem::align_of::<T>())
        {
            return None;
        }
        Some(Slice::Mapped {
            map: self.map.clone(),
            off,
            len: len / size,
        })
    }
}

/// Remove a generation's vector index files (all of them with `name` = `None`).
pub fn remove(dir: &Path, name: Option<&str>) {
    match name {
        Some(n) => {
            let _ = std::fs::remove_file(file_of(dir, n));
        }
        None => {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_damage() {
        let d = tempfile::tempdir().unwrap();
        let p = file_of(d.path(), "x");
        let id = Identity {
            config_hash: 7,
            base_seq: 3,
            quads: 10,
            terms: 20,
        };
        let meta = [1u64, 2, 3];
        let ids = [[1u64, 2, 3], [4, 5, 6]];
        let data = [1.0f32, 2.0, 3.0];
        write(&p, &id, 2, &[Out::of(&meta), Out::of(&ids), Out::of(&data)]).unwrap();
        let m = Mapped::open(&p, Some(&id), true).unwrap();
        assert_eq!(m.rows, 2);
        assert_eq!(&*m.slice::<[u64; 3]>(1).unwrap(), &ids);
        assert_eq!(&*m.slice::<f32>(2).unwrap(), &data);
        let other = Identity { base_seq: 4, ..id };
        assert!(matches!(
            Mapped::open(&p, Some(&other), false),
            Err(Problem::Unusable(_))
        ));
        // a flipped id byte fails the open checksum
        let mut b = std::fs::read(&p).unwrap();
        let (o, _) = m.table[1];
        b[o] ^= 1;
        std::fs::write(&p, &b).unwrap();
        assert!(Mapped::open(&p, None, false).is_err());
        b.truncate(b.len() - 3);
        std::fs::write(&p, &b).unwrap();
        assert!(Mapped::open(&p, None, false).is_err());
        assert_eq!(
            Mapped::open(&d.path().join("none"), None, false).err(),
            Some(Problem::Missing)
        );
    }
}
