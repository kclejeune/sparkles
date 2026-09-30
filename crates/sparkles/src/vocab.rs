//! Term dictionaries.
//!
//! * [`Vocab`] — the immutable **base vocabulary** built by the bulk loader / compaction.
//!   Keys are sorted, so id order equals key order (QLever-style), which lets range and
//!   prefix filters work on ids. Stored front-coded (blocks of [`FC_BLOCK`] keys, each key
//!   stored as `shared-prefix-len, suffix`) and memory-mapped.
//! * [`DeltaVocab`] — append-only dictionary for terms introduced by SPARQL Update. Ids
//!   are in insertion order. Persisted as a length-prefixed log.
//! * [`LocalVocab`] — per-query dictionary for computed terms.

use crate::error::Result;
use memmap2::Mmap;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

pub const FC_BLOCK: usize = 16;

pub(crate) fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
pub(crate) fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

enum Bytes {
    Map(Mmap),
    Vec(Vec<u8>),
}
impl Bytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => m,
            Bytes::Vec(v) => v,
        }
    }
    fn open(path: &Path) -> Result<Bytes> {
        let f = File::open(path)?;
        if f.metadata()?.len() == 0 {
            return Ok(Bytes::Vec(Vec::new()));
        }
        // SAFETY: index files are immutable once written; generations are never
        // modified in place.
        Ok(Bytes::Map(unsafe { Mmap::map(&f)? }))
    }
}

/// Sorted, front-coded, memory-mapped vocabulary.
pub struct Vocab {
    data: Bytes,
    offsets: Bytes,
    len: u64,
    /// id of the first IRI key: literal keys (`"…`) sort before triple-term keys (`(…`),
    /// which sort before IRI keys (`<…`)
    first_iri: u64,
    first_triple: u64,
}

impl Vocab {
    pub fn empty() -> Vocab {
        Vocab {
            data: Bytes::Vec(Vec::new()),
            offsets: Bytes::Vec(Vec::new()),
            len: 0,
            first_iri: 0,
            first_triple: 0,
        }
    }

    pub fn open(dir: &Path) -> Result<Vocab> {
        let data = Bytes::open(&dir.join("vocab.dat"))?;
        let offsets = Bytes::open(&dir.join("vocab.off"))?;
        let off = offsets.as_slice();
        let len = if off.len() >= 8 {
            u64::from_le_bytes(off[off.len() - 8..].try_into().unwrap())
        } else {
            0
        };
        let mut v = Vocab {
            data,
            offsets,
            len,
            first_iri: 0,
            first_triple: 0,
        };
        v.first_iri = match v.find(b"<") {
            Ok(i) | Err(i) => i,
        };
        v.first_triple = match v.find(b"(") {
            Ok(i) | Err(i) => i,
        };
        Ok(v)
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.len
    }
    /// Is base id `id` an IRI (as opposed to a literal)? O(1) thanks to the sort order.
    #[inline]
    pub fn is_iri(&self, id: u64) -> bool {
        id >= self.first_iri
    }
    /// Is base id `id` an RDF 1.2 triple term?
    #[inline]
    pub fn is_triple(&self, id: u64) -> bool {
        id >= self.first_triple && id < self.first_iri
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn disk_bytes(&self) -> u64 {
        (self.data.as_slice().len() + self.offsets.as_slice().len()) as u64
    }

    #[inline]
    fn num_blocks(&self) -> usize {
        // last 8 bytes store the term count
        (self.offsets.as_slice().len() / 8).saturating_sub(1)
    }
    #[inline]
    fn block_offset(&self, b: usize) -> usize {
        let o = self.offsets.as_slice();
        u64::from_le_bytes(o[b * 8..b * 8 + 8].try_into().unwrap()) as usize
    }

    /// First key of block `b` (stored uncompressed).
    fn block_first(&self, b: usize) -> &[u8] {
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        let _shared = read_varint(data, &mut pos);
        let len = read_varint(data, &mut pos) as usize;
        &data[pos..pos + len]
    }

    /// Decode a whole block, invoking `f(index_in_block, key)`; stops when `f` returns false.
    fn scan_block(&self, b: usize, mut f: impl FnMut(usize, &[u8]) -> bool) {
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        let mut key: Vec<u8> = Vec::with_capacity(64);
        for i in 0..n {
            let shared = read_varint(data, &mut pos) as usize;
            let len = read_varint(data, &mut pos) as usize;
            key.truncate(shared);
            key.extend_from_slice(&data[pos..pos + len]);
            pos += len;
            if !f(i, &key) {
                return;
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        if id >= self.len {
            return None;
        }
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let mut out = None;
        self.scan_block(b, |i, k| {
            if i == want {
                out = Some(k.to_vec());
                false
            } else {
                true
            }
        });
        out
    }

    /// Decode many ids (sorted ascending, deduplicated), touching each front-coded
    /// block once: `f(id, key)`.
    pub fn get_sorted(&self, ids: &[u64], mut f: impl FnMut(u64, &[u8])) {
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            if id >= self.len {
                break;
            }
            let b = (id as usize) / FC_BLOCK;
            let end = ((b + 1) * FC_BLOCK) as u64;
            let j = i + ids[i..].partition_point(|&x| x < end);
            let wanted = &ids[i..j];
            let last = (wanted[wanted.len() - 1] as usize) % FC_BLOCK;
            let mut w = 0;
            self.scan_block(b, |pos, k| {
                if w < wanted.len() && (wanted[w] as usize) % FC_BLOCK == pos {
                    f(wanted[w], k);
                    w += 1;
                }
                pos < last
            });
            i = j;
        }
    }

    /// Binary search: `Ok(id)` if present, `Err(insertion point)` otherwise.
    pub fn find(&self, key: &[u8]) -> std::result::Result<u64, u64> {
        let nb = self.num_blocks();
        if nb == 0 {
            return Err(0);
        }
        // last block whose first key <= key
        let (mut lo, mut hi) = (0usize, nb);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.block_first(mid) <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return Err(0);
        }
        let b = lo - 1;
        let mut result = Err(((b + 1) * FC_BLOCK).min(self.len as usize) as u64);
        self.scan_block(b, |i, k| match k.cmp(key) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => {
                result = Ok((b * FC_BLOCK + i) as u64);
                false
            }
            std::cmp::Ordering::Greater => {
                result = Err((b * FC_BLOCK + i) as u64);
                false
            }
        });
        result
    }

    /// Id range `[lo, hi)` of keys with the given byte prefix.
    pub fn prefix_range(&self, prefix: &[u8]) -> (u64, u64) {
        let lo = match self.find(prefix) {
            Ok(i) | Err(i) => i,
        };
        let mut upper = prefix.to_vec();
        // increment the prefix to get the exclusive upper bound
        while let Some(last) = upper.pop() {
            if last < 0xFF {
                upper.push(last + 1);
                let hi = match self.find(&upper) {
                    Ok(i) | Err(i) => i,
                };
                return (lo, hi);
            }
        }
        (lo, self.len)
    }

    /// Iterate all keys in order.
    pub fn for_each(&self, mut f: impl FnMut(u64, &[u8])) {
        for b in 0..self.num_blocks() {
            self.scan_block(b, |i, k| {
                f((b * FC_BLOCK + i) as u64, k);
                true
            });
        }
    }
}

/// Streaming writer for a sorted vocabulary.
pub struct VocabWriter {
    data: BufWriter<File>,
    offsets: BufWriter<File>,
    pos: u64,
    count: u64,
    prev: Vec<u8>,
    buf: Vec<u8>,
}

impl VocabWriter {
    pub fn create(dir: &Path) -> Result<VocabWriter> {
        Ok(VocabWriter {
            data: BufWriter::with_capacity(1 << 20, File::create(dir.join("vocab.dat"))?),
            offsets: BufWriter::new(File::create(dir.join("vocab.off"))?),
            pos: 0,
            count: 0,
            prev: Vec::new(),
            buf: Vec::with_capacity(256),
        })
    }

    /// Append the next key; keys must be pushed in strictly increasing order.
    /// Returns the assigned id.
    pub fn push(&mut self, key: &[u8]) -> Result<u64> {
        debug_assert!(self.count == 0 || key > self.prev.as_slice());
        self.buf.clear();
        let shared = if (self.count as usize).is_multiple_of(FC_BLOCK) {
            self.offsets.write_all(&self.pos.to_le_bytes())?;
            0
        } else {
            self.prev
                .iter()
                .zip(key)
                .take_while(|(a, b)| a == b)
                .count()
        };
        write_varint(&mut self.buf, shared as u64);
        write_varint(&mut self.buf, (key.len() - shared) as u64);
        self.buf.extend_from_slice(&key[shared..]);
        self.data.write_all(&self.buf)?;
        self.pos += self.buf.len() as u64;
        self.prev.clear();
        self.prev.extend_from_slice(key);
        let id = self.count;
        self.count += 1;
        Ok(id)
    }

    pub fn finish(mut self) -> Result<u64> {
        self.offsets.write_all(&self.count.to_le_bytes())?;
        self.data.flush()?;
        self.offsets.flush()?;
        self.data.get_ref().sync_all()?;
        self.offsets.get_ref().sync_all()?;
        Ok(self.count)
    }
}

/// An append-only dictionary; used for the persisted delta vocabulary and for per-query
/// local vocabularies.
#[derive(Default)]
pub struct AppendVocab {
    keys: Vec<Arc<[u8]>>,
    map: FxHashMap<Arc<[u8]>, u64>,
    /// total key length
    key_bytes: usize,
}

impl AppendVocab {
    pub fn len(&self) -> u64 {
        self.keys.len() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn get(&self, id: u64) -> Option<&[u8]> {
        self.keys.get(id as usize).map(|k| &**k)
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        self.map.get(key).copied()
    }
    pub fn insert(&mut self, key: &[u8]) -> (u64, bool) {
        if let Some(&i) = self.map.get(key) {
            return (i, false);
        }
        let k: Arc<[u8]> = key.into();
        let i = self.keys.len() as u64;
        self.key_bytes += k.len();
        self.keys.push(k.clone());
        self.map.insert(k, i);
        (i, true)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.keys.iter().enumerate().map(|(i, k)| (i as u64, &**k))
    }
    pub fn bytes(&self) -> usize {
        self.key_bytes + self.keys.len() * 48
    }
}

/// The persisted, append-only delta vocabulary (terms introduced by updates).
///
/// Readers and the single writer share it through an `RwLock`; ids only ever grow, so a
/// reader's snapshot remains valid while the writer appends.
pub struct DeltaVocab {
    inner: RwLock<AppendVocab>,
    file: Option<parking_lot::Mutex<BufWriter<File>>>,
}

impl DeltaVocab {
    pub fn in_memory() -> DeltaVocab {
        DeltaVocab {
            inner: RwLock::new(AppendVocab::default()),
            file: None,
        }
    }

    /// The delta vocabulary of a sealed generation, for reading: no append handle, and a
    /// torn tail is ignored rather than truncated.
    pub fn open_read_only(path: &Path) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        if path.exists() {
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            let mut pos = 0;
            while pos + 4 <= buf.len() {
                let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
                if pos + 4 + len > buf.len() {
                    break;
                }
                v.insert(&buf[pos + 4..pos + 4 + len]);
                pos += 4 + len;
            }
        }
        Ok(DeltaVocab {
            inner: RwLock::new(v),
            file: None,
        })
    }

    pub fn open(path: &Path) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        if path.exists() {
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            let mut pos = 0;
            while pos + 4 <= buf.len() {
                let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
                if pos + 4 + len > buf.len() {
                    break; // torn write at the tail — ignore
                }
                v.insert(&buf[pos + 4..pos + 4 + len]);
                pos += 4 + len;
            }
            // truncate a torn tail so future appends are well-formed
            if pos != buf.len() {
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(pos as u64)?;
            }
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(DeltaVocab {
            inner: RwLock::new(v),
            file: Some(parking_lot::Mutex::new(BufWriter::new(f))),
        })
    }

    pub fn len(&self) -> u64 {
        self.inner.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        self.inner.read().get(id).map(|k| k.to_vec())
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        self.inner.read().find(key)
    }
    pub fn with<R>(&self, f: impl FnOnce(&AppendVocab) -> R) -> R {
        f(&self.inner.read())
    }

    /// Insert (writer only). Newly added keys are buffered for the next [`sync`](Self::sync).
    pub fn insert(&self, key: &[u8]) -> Result<u64> {
        let (id, new) = self.inner.write().insert(key);
        if new && let Some(f) = &self.file {
            let mut f = f.lock();
            f.write_all(&(key.len() as u32).to_le_bytes())?;
            f.write_all(key)?;
        }
        Ok(id)
    }

    pub fn sync(&self) -> Result<()> {
        if let Some(f) = &self.file {
            let mut f = f.lock();
            f.flush()?;
            f.get_ref().sync_data()?;
        }
        Ok(())
    }

    /// Write buffered entries to the file (without `fsync`) and return its length in
    /// bytes: every entry inserted so far lies within it. 0 for an in-memory vocabulary.
    /// For backups, which read the file up to this length through their own handle.
    pub fn flush(&self) -> Result<u64> {
        match &self.file {
            Some(f) => {
                let mut f = f.lock();
                f.flush()?;
                Ok(f.get_ref().metadata()?.len())
            }
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_sorted_matches_get() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for i in 0..1000 {
            w.push(format!("<http://x/{i:05}").as_bytes()).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        let ids: Vec<u64> = (0..1000).filter(|i| i % 7 == 0 || i % 16 == 15).collect();
        let mut got = Vec::new();
        v.get_sorted(&ids, |i, k| got.push((i, k.to_vec())));
        assert_eq!(got.len(), ids.len());
        for (i, k) in got {
            assert_eq!(v.get(i).unwrap(), k);
        }
    }

    #[test]
    fn front_coded_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut keys: Vec<Vec<u8>> = (0..1000)
            .map(|i| format!("<http://example.org/resource/{i:05}").into_bytes())
            .collect();
        keys.push(b"\"zzz\xff".to_vec());
        keys.sort();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for k in &keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        assert_eq!(v.len(), keys.len() as u64);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(v.get(i as u64).unwrap(), *k);
            assert_eq!(v.find(k), Ok(i as u64));
        }
        assert!(v.find(b"<http://example.org/resource/00010x").is_err());
        let (lo, hi) = v.prefix_range(b"<http://example.org/resource/001");
        assert_eq!(hi - lo, 100);
    }
}
