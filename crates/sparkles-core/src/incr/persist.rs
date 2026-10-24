//! Checkpoints of a circuit's state (spec F13 §8.4), and the same encoding used to move
//! a state from one id space to another.
//!
//! Ids differ between generations, so a checkpoint stores every term as its vocabulary
//! key, the form the change log uses, and loading maps each key to an id of the snapshot
//! being loaded into. A generation switch (compaction) moves the in-memory state the
//! same way, through an encoding that never reaches the disk.
//!
//! **File.** `<root>/views/{name}/main/state-{commit}.bin`, 20-digit commit:
//!
//! | bytes | field |
//! |---|---|
//! | 8 | magic `SPKVIEW\0` |
//! | 4 | format (u32 LE), 1 |
//! | 4 | flags (u32 LE), 0 |
//! | 16 | the dataset's UUID |
//! | 8 | the commit the state reflects (u64 LE) |
//! | 8 | the circuit's fingerprint (u64 LE): a hash of the definition and the compiler's version |
//! | 8 | body length (u64 LE) |
//! | 4 | CRC-32 of the body |
//! | … | body: the term table (varint count, then varint length and key each), then the operators' states |
//!
//! A reader refuses a format it does not know with [`Error::Unsupported`], and the view
//! is then built from scratch while the file is left in place.

use super::terms::Terms;
use super::zset::{Row, ZSet};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::store::Snapshot;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"SPKVIEW\0";
/// The checkpoint format this release writes and reads.
pub const FORMAT: u32 = 1;
const HEADER: usize = 8 + 4 + 4 + 16 + 8 + 8 + 8 + 4;

/// Encodes a state, mapping ids to the keys of their terms.
pub struct Writer<'a> {
    snap: &'a Snapshot,
    terms: &'a Terms,
    index: FxHashMap<Id, u64>,
    keys: Vec<Vec<u8>>,
    body: Vec<u8>,
}

impl<'a> Writer<'a> {
    pub fn new(snap: &'a Snapshot, terms: &'a Terms) -> Writer<'a> {
        Writer {
            snap,
            terms,
            index: FxHashMap::default(),
            keys: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn u64(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.body.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.body.push(v as u8);
    }

    pub fn i64(&mut self, v: i64) {
        self.u64(((v << 1) ^ (v >> 63)) as u64);
    }

    pub fn i128(&mut self, v: i128) {
        self.body.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.body.extend_from_slice(b);
    }

    /// An id, as the number of its term in the table (0 for unbound).
    pub fn id(&mut self, id: Id) -> Result<()> {
        if id.is_undef() {
            self.u64(0);
            return Ok(());
        }
        let n = match self.index.get(&id) {
            Some(&n) => n,
            None => {
                let key = self.terms.key(self.snap, id).ok_or_else(|| {
                    Error::Corrupt(format!(
                        "a view's state holds an id with no term ({:#x})",
                        id.0
                    ))
                })?;
                self.keys.push(key);
                let n = self.keys.len() as u64;
                self.index.insert(id, n);
                n
            }
        };
        self.u64(n);
        Ok(())
    }

    pub fn row(&mut self, r: &[Id]) -> Result<()> {
        self.u64(r.len() as u64);
        for &i in r {
            self.id(i)?;
        }
        Ok(())
    }

    pub fn zset(&mut self, z: &ZSet) -> Result<()> {
        self.u64(z.len() as u64);
        for (r, w) in z.iter() {
            self.row(r)?;
            self.i64(w);
        }
        Ok(())
    }

    /// The term table followed by the body.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() + self.keys.len() * 16 + 8);
        let mut w = Writer {
            snap: self.snap,
            terms: self.terms,
            index: FxHashMap::default(),
            keys: Vec::new(),
            body: Vec::new(),
        };
        w.u64(self.keys.len() as u64);
        for k in &self.keys {
            w.bytes(k);
        }
        out.extend_from_slice(&w.body);
        out.extend_from_slice(&self.body);
        out
    }
}

/// Decodes a state into the id space of a snapshot and a fresh set of local terms.
pub struct Reader<'a> {
    body: &'a [u8],
    at: usize,
    ids: Vec<Id>,
}

fn short() -> Error {
    Error::Corrupt("a view's state ends early".into())
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], snap: &Snapshot, terms: &mut Terms) -> Result<Reader<'a>> {
        let mut r = Reader {
            body: data,
            at: 0,
            ids: vec![Id::UNDEF],
        };
        let n = r.u64()?;
        for _ in 0..n {
            let k = r.bytes()?.to_vec();
            r.ids.push(terms.intern_key(snap, &k));
        }
        Ok(r)
    }

    pub fn u64(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = *self.body.get(self.at).ok_or_else(short)?;
            self.at += 1;
            if shift >= 64 {
                return Err(Error::Corrupt(
                    "a view's state has a malformed number".into(),
                ));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    pub fn i64(&mut self) -> Result<i64> {
        let v = self.u64()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    pub fn i128(&mut self) -> Result<i128> {
        let b = self.body.get(self.at..self.at + 16).ok_or_else(short)?;
        self.at += 16;
        Ok(i128::from_le_bytes(b.try_into().expect("16 bytes")))
    }

    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u64()? as usize;
        let b = self
            .body
            .get(self.at..self.at.checked_add(n).ok_or_else(short)?)
            .ok_or_else(short)?;
        self.at += n;
        Ok(b)
    }

    pub fn id(&mut self) -> Result<Id> {
        let n = self.u64()? as usize;
        self.ids
            .get(n)
            .copied()
            .ok_or_else(|| Error::Corrupt("a view's state names a term it does not hold".into()))
    }

    pub fn row(&mut self) -> Result<Row> {
        let n = self.u64()? as usize;
        if n > 1 << 16 {
            return Err(Error::Corrupt(
                "a view's state has an impossible row".into(),
            ));
        }
        (0..n).map(|_| self.id()).collect()
    }

    pub fn zset(&mut self) -> Result<ZSet> {
        let n = self.u64()?;
        let mut z = ZSet::new();
        for _ in 0..n {
            let r = self.row()?;
            let w = self.i64()?;
            z.add(r, w);
        }
        Ok(z)
    }

    pub fn done(&self) -> bool {
        self.at == self.body.len()
    }
}

/// The header of a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub dataset: uuid::Uuid,
    pub commit: u64,
    pub fingerprint: u64,
}

fn crc32(b: &[u8]) -> u32 {
    let mut h = flate2::Crc::new();
    h.update(b);
    h.sum()
}

/// The bytes of a checkpoint file.
pub fn encode(h: &Header, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(h.dataset.as_bytes());
    out.extend_from_slice(&h.commit.to_le_bytes());
    out.extend_from_slice(&h.fingerprint.to_le_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(&crc32(body).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// The header and body of a checkpoint file. A format newer than this release reads is
/// [`Error::Unsupported`], and damage is [`Error::Corrupt`].
pub fn decode(b: &[u8]) -> Result<(Header, &[u8])> {
    if b.len() < 16 || &b[..8] != MAGIC {
        return Err(Error::Corrupt("not a view state file".into()));
    }
    let format = u32::from_le_bytes(b[8..12].try_into().expect("4 bytes"));
    if format != FORMAT {
        return Err(Error::Unsupported(format!(
            "the view state has format {format}; Sparkles {} reads format {FORMAT}. It was \
             written by another release",
            env!("CARGO_PKG_VERSION")
        )));
    }
    if b.len() < HEADER {
        return Err(Error::Corrupt(
            "a view state file ends in its header".into(),
        ));
    }
    let u = |r: std::ops::Range<usize>| u64::from_le_bytes(b[r].try_into().expect("8 bytes"));
    let dataset = uuid::Uuid::from_slice(&b[16..32]).map_err(|e| Error::Corrupt(e.to_string()))?;
    let commit = u(32..40);
    let fingerprint = u(40..48);
    let len = u(48..56) as usize;
    let crc = u32::from_le_bytes(b[56..60].try_into().expect("4 bytes"));
    let body = b
        .get(HEADER..HEADER.checked_add(len).ok_or_else(short)?)
        .ok_or_else(|| Error::Corrupt("a view state file is truncated".into()))?;
    if crc32(body) != crc {
        return Err(Error::Corrupt(
            "a view state file fails its checksum".into(),
        ));
    }
    Ok((
        Header {
            dataset,
            commit,
            fingerprint,
        },
        body,
    ))
}

/// The name of the checkpoint of commit `commit`.
pub fn file_name(commit: u64) -> String {
    format!("state-{commit:020}.bin")
}

/// The checkpoints in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut out: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name.strip_prefix("state-")?.strip_suffix(".bin")?;
            Some((n.parse().ok()?, e.path()))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// Write a checkpoint atomically, then remove the older ones this release can read.
pub fn write(dir: &Path, h: &Header, body: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(h.commit));
    crate::store::write_atomic(&path, &encode(h, body))?;
    for (c, p) in list(dir) {
        if c == h.commit {
            continue;
        }
        // a file another release wrote is left alone
        let known = std::fs::read(&p)
            .ok()
            .is_some_and(|b| b.len() >= 12 && b[8..12] == FORMAT.to_le_bytes());
        if known {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(())
}
