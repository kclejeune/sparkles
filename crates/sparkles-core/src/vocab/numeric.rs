//! The numeric column of a base vocabulary (`vocab.num`).
//!
//! Numeric literals whose lexical form is not canonical (`"48.85"^^xsd:float`,
//! `"0042"^^xsd:integer`, any `xsd:int` or `xsd:nonNegativeInteger`) cannot be inlined
//! into their id without changing the term, so they live in the vocabulary. A filter or
//! a sort on their value would decode the key of every one of them. This file holds the
//! value of each such literal, so that a comparison reads 8 bytes instead.
//!
//! Literal keys sort before every other key, so the literals are the ids
//! `0..covered`. The vocabulary sorts them by lexical form, which interleaves numbers
//! with strings, dates and language-tagged text. Numbers still gather where lexical
//! forms start with a digit, a sign or a point, and long runs of literals hold none
//! (labels, abstracts). The column keeps a list of segments, ranges of ids that hold
//! every number and no run of more than [`GAP`] literals without one. An id outside
//! every segment is not a number, which costs no read. Each id inside a segment has a
//! 4-bit kind, and the numbers have their values:
//!
//! * [`NOT_NUMERIC`]: the literal's value is not a number (a string, a date, an
//!   ill-typed numeric literal…). Comparing it with a number is a type error.
//! * [`UNKNOWN`]: a number the column does not hold, a decimal with more significant
//!   digits than fit into 59 bits. Its key is decoded as before.
//! * [`INTEGER`], [`DECIMAL`], [`FLOAT`], [`DOUBLE`] (bit 3 set): the value is the next
//!   entry of the value array, in id order. An integer is its `i64`, a float the bits of
//!   its `f32`, a double the bits of its `f64` and a decimal its exact value as a
//!   5-bit scale and a 59-bit mantissa.
//!
//! Each value is the one [`Value::from_typed`] parses from the key, so a value read
//! from the column equals the value decoded from the key, with its type, and every
//! comparison, promotion and arithmetic on it gives the same result.
//!
//! The file is derived data, written next to the vocabulary when it is built. A
//! generation without one (built by an older version) decodes its numbers from the
//! vocabulary, as before.
//!
//! Layout, little-endian:
//!
//! ```text
//! magic "SPKVNUM2" | vocabulary length u64 | covered u64 | values u64
//!   | positions u64 | segments u64
//! kinds: ceil(positions / 16) u64 words, position p in bits 4·(p % 16) of word p / 16
//! rank: ceil(positions / 128) u32, the values before each group of 128 positions,
//!   padded to 8 bytes
//! values: one u64 per position of a valued kind, in id order
//! segments: per segment its first id, its end and its first position, u64 each
//! ```
//!
//! The ids of the segments, in order, are the positions `0..positions`.

use super::Bytes;
use crate::error::Result;
use crate::id::KEY_SEP;
use crate::sparql::value::Value;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The column's file name in a generation directory.
pub const FILE: &str = "vocab.num";
const MAGIC: &[u8; 8] = b"SPKVNUM2";
const HEADER: usize = 48;
/// A run of more literals than this without a number ends a segment. It is the ids of
/// one 4 KiB page of kinds.
pub const GAP: u64 = 8192;
/// Positions per rank entry: eight words of kinds, one cache line.
const RANK_IDS: u64 = 128;
const VALUED_BITS: u64 = 0x8888_8888_8888_8888;
/// Read-ahead hints are given once per chunk of this many bytes.
const HINT_CHUNK: usize = 16 << 10;
/// Chunks this close are asked for in one request.
const HINT_GAP: usize = 4;
/// Chunks touching at least one in this many chunks of their span are asked for as the
/// whole span.
const DENSE: usize = 4;

pub const NOT_NUMERIC: u8 = 0;
pub const UNKNOWN: u8 = 1;
pub const INTEGER: u8 = 8;
pub const DECIMAL: u8 = 9;
pub const FLOAT: u8 = 10;
pub const DOUBLE: u8 = 11;

const DEC_SCALE_BITS: u32 = 5;
const DEC_MANTISSA_BITS: u32 = 64 - DEC_SCALE_BITS;

/// What the column says about one id of the vocabulary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Numeric {
    /// a literal whose value is not a number, or a term that is no literal
    NotNumeric,
    /// a number that is not in the column: decode its key
    Unknown,
    Integer(i64),
    Decimal(oxsdatatypes::Decimal),
    Float(f32),
    Double(f64),
}

impl Numeric {
    /// The value decoding the key gives, for a number held by the column.
    #[inline]
    pub fn value(self) -> Option<Value> {
        Some(match self {
            Numeric::Integer(i) => Value::Integer(i.into()),
            Numeric::Decimal(d) => Value::Decimal(d),
            Numeric::Float(f) => Value::Float(f.into()),
            Numeric::Double(d) => Value::Double(d.into()),
            Numeric::NotNumeric | Numeric::Unknown => return None,
        })
    }

    fn of(kind: u8, bits: u64) -> Numeric {
        match kind {
            INTEGER => Numeric::Integer(bits as i64),
            DECIMAL => Numeric::Decimal(unpack_decimal(bits)),
            FLOAT => Numeric::Float(f32::from_bits(bits as u32)),
            DOUBLE => Numeric::Double(f64::from_bits(bits)),
            _ => Numeric::Unknown,
        }
    }
}

/// A decimal as a 5-bit scale and a 59-bit two's complement mantissa, value =
/// mantissa / 10^scale, when it fits. `oxsdatatypes` holds a decimal as an `i128` of
/// 18 fractional digits, so every scale from 0 to 18 is exact.
pub fn pack_decimal(d: oxsdatatypes::Decimal) -> Option<u64> {
    let v = i128::from_be_bytes(d.to_be_bytes());
    let lim = 1i128 << (DEC_MANTISSA_BITS - 1);
    for scale in 0..=18u32 {
        let div = 10i128.pow(18 - scale);
        if v % div == 0 {
            let m = v / div;
            if m < -lim || m >= lim {
                return None;
            }
            return Some(
                ((scale as u64) << DEC_MANTISSA_BITS)
                    | (m as u64 & ((1u64 << DEC_MANTISSA_BITS) - 1)),
            );
        }
    }
    None
}

pub fn unpack_decimal(p: u64) -> oxsdatatypes::Decimal {
    let scale = (p >> DEC_MANTISSA_BITS) as u32;
    let m = (((p << DEC_SCALE_BITS) as i64) >> DEC_SCALE_BITS) as i128;
    let v = m * 10i128.pow(18 - scale.min(18));
    oxsdatatypes::Decimal::from_be_bytes(v.to_be_bytes())
}

/// The kind and value bits of a literal key (`"lexical 0xFF suffix`). The value is the
/// one [`Value::from_key`] decodes.
pub fn classify(key: &[u8]) -> (u8, u64) {
    let Some(sep) = memchr::memrchr(KEY_SEP, key) else {
        return (NOT_NUMERIC, 0);
    };
    let suffix = &key[sep + 1..];
    let Some((b'^', dt)) = suffix.split_first() else {
        // a plain string or a language-tagged string
        return (NOT_NUMERIC, 0);
    };
    let (Ok(lex), Ok(dt)) = (std::str::from_utf8(&key[1..sep]), std::str::from_utf8(dt)) else {
        // `from_key` reads invalid UTF-8 lossily, and no number has U+FFFD in it
        return (NOT_NUMERIC, 0);
    };
    if !crate::sparql::value::numeric_datatype(dt) {
        return (NOT_NUMERIC, 0);
    }
    match Value::from_typed(lex, dt) {
        Value::Integer(i) => (INTEGER, i64::from(i) as u64),
        Value::Decimal(d) => match pack_decimal(d) {
            Some(p) => (DECIMAL, p),
            None => (UNKNOWN, 0),
        },
        Value::Float(f) => (FLOAT, f32::from(f).to_bits() as u64),
        Value::Double(d) => (DOUBLE, f64::from(d).to_bits()),
        _ => (NOT_NUMERIC, 0),
    }
}

/// A memory-mapped `vocab.num`.
pub struct NumColumn {
    bytes: Bytes,
    /// one bit per [`HINT_CHUNK`]: read ahead was asked for
    hinted: Box<[AtomicU64]>,
    covered: u64,
    count: u64,
    rank: usize,
    values: usize,
    /// (first id, end, first position) of each segment
    segments: Box<[(u64, u64, u64)]>,
}

fn word(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The offsets of the rank, the values, the segments and the end of the file.
fn layout(positions: u64, count: u64, segments: u64) -> Option<(usize, usize, usize, usize)> {
    let words = usize::try_from(positions.div_ceil(16)).ok()?;
    let rank = HEADER.checked_add(words.checked_mul(8)?)?;
    let ranks = usize::try_from(positions.div_ceil(RANK_IDS)).ok()?;
    let values = rank.checked_add(ranks.checked_mul(4)?.next_multiple_of(8))?;
    let segs = values.checked_add(usize::try_from(count).ok()?.checked_mul(8)?)?;
    let end = segs.checked_add(usize::try_from(segments).ok()?.checked_mul(24)?)?;
    Some((rank, values, segs, end))
}

impl NumColumn {
    /// Open the column of a vocabulary of `len` ids whose literals are the ids
    /// `0..literals`. `None` when the file is missing, or does not describe this
    /// vocabulary (then numbers are decoded from their keys).
    pub(super) fn open(dir: &Path, len: u64, literals: u64) -> Option<NumColumn> {
        let path = dir.join(FILE);
        let f = File::open(&path).ok()?;
        let bytes = match f.metadata() {
            Ok(m) if m.len() >= HEADER as u64 => Bytes::open(&path, true).ok()?,
            _ => return None,
        };
        let b = bytes.as_slice();
        let (covered, count, positions, nsegs) =
            (word(b, 16), word(b, 24), word(b, 32), word(b, 40));
        let segments = || -> Option<(usize, usize, Box<[(u64, u64, u64)]>)> {
            if b[..8] != *MAGIC
                || word(b, 8) != len
                || covered != literals
                || count > positions
                || positions > covered
            {
                return None;
            }
            let (rank, values, segs, end) = layout(positions, count, nsegs)?;
            if end != b.len() {
                return None;
            }
            let list: Box<[(u64, u64, u64)]> = (0..nsegs as usize)
                .map(|i| {
                    let at = segs + i * 24;
                    (word(b, at), word(b, at + 8), word(b, at + 16))
                })
                .collect();
            // ascending, apart, inside the literals, and their ids are the positions
            let mut next = (0, 0);
            for &(lo, hi, base) in &list {
                if lo < next.0 || hi <= lo || hi > covered || base != next.1 {
                    return None;
                }
                next = (hi, base + (hi - lo));
            }
            (next.1 == positions).then_some((rank, values, list))
        };
        let Some((rank, values, segments)) = segments() else {
            tracing::warn!(
                target: "sparkles::vocab",
                "{} does not match its vocabulary; numbers are decoded from their keys",
                path.display()
            );
            return None;
        };
        let chunks = b.len().div_ceil(HINT_CHUNK);
        Some(NumColumn {
            hinted: (0..chunks.div_ceil(64))
                .map(|_| AtomicU64::new(0))
                .collect(),
            bytes,
            covered,
            count,
            rank,
            values,
            segments,
        })
    }

    /// Ranges of ids that hold every number.
    pub fn segments(&self) -> usize {
        self.segments.len()
    }

    /// Whether base id `id` may be a number. The answer costs no read: an id outside
    /// every segment is not one.
    #[inline]
    pub fn may_hold(&self, id: u64) -> bool {
        self.position(id).is_some()
    }

    /// The position of base id `id` in the kinds, if it is inside a segment.
    #[inline]
    fn position(&self, id: u64) -> Option<u64> {
        let s = &self.segments;
        let i = s.partition_point(|&(_, hi, _)| hi <= id);
        let &(lo, _, base) = s.get(i)?;
        (lo <= id).then(|| base + (id - lo))
    }

    /// Ids covered: the literals of the vocabulary.
    pub fn covered(&self) -> u64 {
        self.covered
    }

    /// Numbers held.
    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn disk_bytes(&self) -> u64 {
        self.bytes.as_slice().len() as u64
    }

    #[inline]
    fn kind_word(&self, w: usize) -> u64 {
        word(self.bytes.as_slice(), HEADER + w * 8)
    }

    #[inline]
    fn rank_at(&self, g: usize) -> u64 {
        let b = self.bytes.as_slice();
        let at = self.rank + g * 4;
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as u64
    }

    /// The kind of base id `id` and, for a valued kind, the index of its value.
    #[inline]
    fn locate(&self, id: u64) -> (u8, u64) {
        let Some(id) = self.position(id) else {
            return (NOT_NUMERIC, 0);
        };
        let w = (id / 16) as usize;
        let shift = (id % 16) * 4;
        let kw = self.kind_word(w);
        let kind = ((kw >> shift) & 0xF) as u8;
        if kind & 8 == 0 {
            return (kind, 0);
        }
        let g = id / RANK_IDS;
        let mut r = self.rank_at(g as usize);
        for x in (g * RANK_IDS / 16) as usize..w {
            r += (self.kind_word(x) & VALUED_BITS).count_ones() as u64;
        }
        r += (kw & VALUED_BITS & ((1u64 << shift) - 1)).count_ones() as u64;
        (kind, r)
    }

    /// What the column says about base id `id`.
    #[inline]
    pub fn get(&self, id: u64) -> Numeric {
        match self.locate(id) {
            (NOT_NUMERIC, _) => Numeric::NotNumeric,
            (k, r) if k & 8 != 0 && r < self.count => {
                Numeric::of(k, word(self.bytes.as_slice(), self.values + r as usize * 8))
            }
            _ => Numeric::Unknown,
        }
    }

    /// Ask once, in the life of the mapping, for each [`HINT_CHUNK`] of the file that
    /// `ranges` (byte ranges in ascending order) touch to be read ahead (see
    /// [`crate::index::io_hints`]). Nearby chunks are asked for in one request. A warm
    /// pass makes no system call, and a cold one finds its pages read by a few large
    /// requests rather than one fault per page.
    fn advise(&self, ranges: impl Iterator<Item = (usize, usize)>) {
        let len = self.bytes.as_slice().len();
        let mut fresh = Vec::new();
        for (s, e) in ranges {
            let (c0, c1) = (s / HINT_CHUNK, e.min(len).div_ceil(HINT_CHUNK));
            for c in c0..c1 {
                let (word, bit) = (&self.hinted[c / 64], 1u64 << (c % 64));
                if word.load(Relaxed) & bit != 0 || word.fetch_or(bit, Relaxed) & bit != 0 {
                    continue;
                }
                fresh.push(c);
            }
        }
        let (Some(&first), Some(&last)) = (fresh.first(), fresh.last()) else {
            return;
        };
        // When the chunks cover a good part of their span, one request for the whole
        // span reads faster than many small ones, at the cost of the gaps between them.
        let gap = if fresh.len() * DENSE >= last + 1 - first {
            usize::MAX
        } else {
            HINT_GAP
        };
        let mut cur = (first, first + 1);
        for &c in &fresh[1..] {
            if c >= cur.0 && c <= cur.1.saturating_add(gap) {
                cur.1 = cur.1.max(c + 1);
            } else {
                self.bytes
                    .will_need(cur.0 * HINT_CHUNK, (cur.1 * HINT_CHUNK).min(len));
                cur = (c, c + 1);
            }
        }
        self.bytes
            .will_need(cur.0 * HINT_CHUNK, (cur.1 * HINT_CHUNK).min(len));
    }

    /// What the column says about each of `ids` (best sorted ascending), reading a cold
    /// column in two rounds of large requests: first the kinds and rank entries of the
    /// ids, then the values they point to.
    pub fn get_many(&self, ids: &[u64]) -> Vec<Numeric> {
        let hints = crate::index::io_hints() && ids.len() > 1;
        if hints {
            let inside = || ids.iter().filter_map(|&id| self.position(id));
            self.advise(inside().map(|id| {
                let first = (id / RANK_IDS) * RANK_IDS / 16;
                (
                    HEADER + first as usize * 8,
                    HEADER + (id / 16 + 1) as usize * 8,
                )
            }));
            self.advise(inside().map(|id| {
                let at = self.rank + (id / RANK_IDS) as usize * 4;
                (at, at + 4)
            }));
        }
        let located: Vec<(u8, u64)> = ids.iter().map(|&id| self.locate(id)).collect();
        if hints {
            self.advise(
                located
                    .iter()
                    .filter(|&&(k, r)| k & 8 != 0 && r < self.count)
                    .map(|&(_, r)| {
                        let at = self.values + r as usize * 8;
                        (at, at + 8)
                    }),
            );
        }
        let b = self.bytes.as_slice();
        located
            .into_iter()
            .map(|(k, r)| match k {
                NOT_NUMERIC => Numeric::NotNumeric,
                k if k & 8 != 0 && r < self.count => {
                    Numeric::of(k, word(b, self.values + r as usize * 8))
                }
                _ => Numeric::Unknown,
            })
            .collect()
    }
}

/// Builds `vocab.num` while a vocabulary is written, one key at a time in id order.
pub(super) struct NumWriter {
    kinds: BufWriter<crate::disk::WritebackFile>,
    values: BufWriter<File>,
    word: u64,
    /// literal ids pushed
    covered: u64,
    /// kinds written
    pos: u64,
    count: u64,
    rank: Vec<u32>,
    /// segments closed, and the open one's first id and position
    segments: Vec<(u64, u64, u64)>,
    open: Option<(u64, u64)>,
    /// the last id of a number
    last: u64,
    /// a non-literal key was pushed: every later key is one too
    done: bool,
    /// too many numbers for the rank's `u32`: no column is written
    failed: bool,
}

impl NumWriter {
    pub(super) fn create(dir: &Path) -> Result<NumWriter> {
        let mut kinds = BufWriter::with_capacity(
            1 << 20,
            crate::disk::WritebackFile::new(File::create(dir.join(format!("{FILE}.tmp")))?),
        );
        kinds.write_all(&[0; HEADER])?;
        let values = BufWriter::with_capacity(
            1 << 20,
            File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(dir.join(format!("{FILE}.values.tmp")))?,
        );
        Ok(NumWriter {
            kinds,
            values,
            word: 0,
            covered: 0,
            pos: 0,
            count: 0,
            rank: Vec::new(),
            segments: Vec::new(),
            open: None,
            last: 0,
            done: false,
            failed: false,
        })
    }

    /// The next key of the vocabulary.
    #[inline]
    pub(super) fn push(&mut self, key: &[u8]) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if key.first() != Some(&b'"') {
            self.done = true;
            return Ok(());
        }
        let (kind, bits) = classify(key);
        self.push_kind(kind, bits)
    }

    fn push_kind(&mut self, kind: u8, bits: u64) -> Result<()> {
        let id = self.covered;
        self.covered += 1;
        if kind == NOT_NUMERIC {
            return Ok(());
        }
        match self.open {
            Some(_) if id - self.last - 1 <= GAP => {
                for _ in self.last + 1..id {
                    self.put(NOT_NUMERIC, 0)?;
                }
            }
            open => {
                if let Some((lo, base)) = open {
                    self.segments.push((lo, self.last + 1, base));
                }
                self.open = Some((id, self.pos));
            }
        }
        self.last = id;
        self.put(kind, bits)
    }

    /// The kind at the next position.
    fn put(&mut self, kind: u8, bits: u64) -> Result<()> {
        if self.pos.is_multiple_of(RANK_IDS) {
            match u32::try_from(self.count) {
                Ok(r) => self.rank.push(r),
                Err(_) => self.failed = true,
            }
        }
        self.word |= (kind as u64) << ((self.pos % 16) * 4);
        if kind & 8 != 0 {
            self.values.write_all(&bits.to_le_bytes())?;
            self.count += 1;
        }
        self.pos += 1;
        if self.pos.is_multiple_of(16) {
            self.kinds.write_all(&self.word.to_le_bytes())?;
            self.word = 0;
        }
        Ok(())
    }

    /// Write the file for a vocabulary of `len` keys, and return the numbers in it.
    /// Without a single number, no file is written.
    pub(super) fn finish(mut self, dir: &Path, len: u64) -> Result<u64> {
        let tmp = dir.join(format!("{FILE}.tmp"));
        let values_tmp = dir.join(format!("{FILE}.values.tmp"));
        let result = (|| -> Result<bool> {
            if self.failed || self.count == 0 {
                return Ok(false);
            }
            if let Some((lo, base)) = self.open {
                self.segments.push((lo, self.last + 1, base));
            }
            if !self.pos.is_multiple_of(16) {
                self.kinds.write_all(&self.word.to_le_bytes())?;
            }
            let mut rank = Vec::with_capacity((self.rank.len() * 4).next_multiple_of(8));
            for r in &self.rank {
                rank.extend_from_slice(&r.to_le_bytes());
            }
            rank.resize(rank.len().next_multiple_of(8), 0);
            self.kinds.write_all(&rank)?;
            let mut values = self.values.into_inner().map_err(|e| e.into_error())?;
            values.seek(SeekFrom::Start(0))?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = values.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                self.kinds.write_all(&buf[..n])?;
            }
            for &(lo, hi, base) in &self.segments {
                for x in [lo, hi, base] {
                    self.kinds.write_all(&x.to_le_bytes())?;
                }
            }
            let mut header = [0u8; HEADER];
            header[..8].copy_from_slice(MAGIC);
            header[8..16].copy_from_slice(&len.to_le_bytes());
            header[16..24].copy_from_slice(&self.covered.to_le_bytes());
            header[24..32].copy_from_slice(&self.count.to_le_bytes());
            header[32..40].copy_from_slice(&self.pos.to_le_bytes());
            header[40..48].copy_from_slice(&(self.segments.len() as u64).to_le_bytes());
            self.kinds.flush()?;
            let w = self.kinds.into_inner().map_err(|e| e.into_error())?;
            let mut f = w.get_ref();
            f.seek(SeekFrom::Start(0))?;
            f.write_all(&header)?;
            f.sync_all()?;
            Ok(true)
        })();
        let _ = std::fs::remove_file(&values_tmp);
        match result {
            Ok(true) => {
                std::fs::rename(&tmp, dir.join(FILE))?;
                Ok(self.count)
            }
            Ok(false) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(0)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::id::{inline_id, term_key};
    use crate::vocab::{Vocab, VocabWriter};
    use oxrdf::{Literal, NamedNode, Term};

    /// Equal values of equal types, NaN and the sign of zero included.
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Integer(x), Value::Integer(y)) => x == y,
            (Value::Decimal(x), Value::Decimal(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => {
                f32::from(*x).to_bits() == f32::from(*y).to_bits()
            }
            (Value::Double(x), Value::Double(y)) => {
                f64::from(*x).to_bits() == f64::from(*y).to_bits()
            }
            _ => false,
        }
    }

    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    /// Literals at the edges: special floats, signed zeros, huge decimals, integers
    /// past 2^53 and past i64, ill-typed numbers, strings that look like numbers, and
    /// other datatypes.
    pub(crate) fn edge_literals() -> Vec<Term> {
        let typed = |lex: &str, dt: &str| -> Term {
            Literal::new_typed_literal(lex, NamedNode::new_unchecked(format!("{XSD}{dt}"))).into()
        };
        let mut v = Vec::new();
        for dt in ["float", "double"] {
            for lex in [
                "NaN",
                "nan",
                "INF",
                "-INF",
                "+INF",
                "inf",
                "infinity",
                "-0",
                "-0.0",
                "0",
                "+0",
                "1e400",
                "-1e400",
                "1e-400",
                "3.4028235e38",
                "3.5e38",
                "1.17549435e-38",
                "48.8566",
                "48.85660",
                "2.3522",
                "0048.8566",
                "4.88566E1",
                ".5",
                "5.",
                "1e",
                "",
                " 48",
                "48 ",
                "abc",
                "0x10",
                "1_000",
            ] {
                v.push(typed(lex, dt));
            }
        }
        for lex in [
            "0",
            "-0",
            "+0",
            "007",
            "9007199254740993",
            "+9007199254740993",
            "-9007199254740993",
            "9223372036854775807",
            "-9223372036854775808",
            "9223372036854775808",
            "99999999999999999999",
            "1.0",
            "1e3",
            "abc",
            "",
            " 1",
        ] {
            for dt in [
                "integer",
                "int",
                "long",
                "short",
                "byte",
                "nonNegativeInteger",
                "positiveInteger",
                "negativeInteger",
                "nonPositiveInteger",
                "unsignedLong",
                "unsignedInt",
                "unsignedShort",
                "unsignedByte",
            ] {
                v.push(typed(lex, dt));
            }
        }
        for lex in [
            "0.0",
            "-0.0",
            "1.50",
            "+1.5",
            "-.5",
            "5.",
            "48.856600",
            "0.000000000000000001",
            "0.0000000000000000001",
            "123456789012345678.5",
            "288230376151711743",
            "288230376151711744",
            "-288230376151711744",
            "-288230376151711745",
            "28823037615171174.3",
            "79228162514264337593543950335",
            "1e3",
            "abc",
            "NaN",
            "INF",
        ] {
            v.push(typed(lex, "decimal"));
        }
        for (lex, dt) in [
            ("1", "boolean"),
            ("true", "boolean"),
            ("2024-01-01", "date"),
            ("2024-01-01T00:00:00Z", "dateTime"),
            ("12", "string"),
            ("PT1H", "duration"),
            ("12", "gYear"),
        ] {
            v.push(typed(lex, dt));
        }
        v.push(Literal::new_simple_literal("48.5").into());
        v.push(Literal::new_language_tagged_literal_unchecked("48.5", "en").into());
        v.push(
            Literal::new_typed_literal("48.5", NamedNode::new_unchecked("http://ex.org/dt")).into(),
        );
        v.push(NamedNode::new_unchecked("http://ex.org/48").into());
        v
    }

    #[test]
    fn decimals_pack_exactly() {
        for lex in [
            "0",
            "-0.5",
            "48.8566",
            "0.000000000000000001",
            "-0.000000000000000001",
            "288230376151711743",
            "-288230376151711744",
            "2882303761.51711743",
        ] {
            let d: oxsdatatypes::Decimal = lex.parse().unwrap();
            assert_eq!(unpack_decimal(pack_decimal(d).unwrap()), d, "{lex}");
        }
        for lex in ["288230376151711744", "-288230376151711745"] {
            let d: oxsdatatypes::Decimal = lex.parse().unwrap();
            assert!(pack_decimal(d).is_none(), "{lex}");
        }
    }

    /// The column gives the value of every literal that decoding its key gives, and
    /// says "not a number" exactly when decoding gives no number.
    #[test]
    fn the_column_matches_decoding() {
        let mut keys: Vec<Vec<u8>> = edge_literals()
            .iter()
            .filter(|t| inline_id(t).is_none())
            .map(term_key)
            .collect();
        // enough keys for several rank groups and a partial last word
        for i in 0..1000 {
            keys.push(term_key(
                &Literal::new_typed_literal(format!("{i}.50"), oxrdf::vocab::xsd::FLOAT).into(),
            ));
            keys.push(term_key(
                &Literal::new_simple_literal(format!("{i}")).into(),
            ));
        }
        // strings between the digits and "inf": two segments
        for i in 0..GAP + 100 {
            keys.push(term_key(
                &Literal::new_simple_literal(format!("a{i:06}")).into(),
            ));
        }
        keys.sort();
        keys.dedup();
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for k in &keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
        assert!(dir.path().join(FILE).exists());
        let v = Vocab::open(dir.path()).unwrap();
        let num = v.numeric().expect("numeric column");
        assert_eq!(num.covered(), v.first_triple);
        assert!(num.segments() >= 2, "{}", num.segments());
        let (mut numbers, mut unknown) = (0, 0);
        for (id, k) in keys.iter().enumerate() {
            let decoded = Value::from_key(k);
            let shown = String::from_utf8_lossy(k);
            match num.get(id as u64) {
                Numeric::NotNumeric => assert!(!decoded.is_numeric(), "{shown}: {decoded:?}"),
                Numeric::Unknown => {
                    assert!(decoded.is_numeric(), "{shown}");
                    unknown += 1;
                }
                n => {
                    let value = n.value().unwrap();
                    assert!(same(&value, &decoded), "{shown}: {value:?} vs {decoded:?}");
                    numbers += 1;
                }
            }
        }
        assert_eq!(num.count(), numbers);
        assert!(unknown > 0, "a decimal too long for the column");
        assert!(numbers > 1000);
        // an IRI, and ids past the end
        assert_eq!(num.get(v.len() - 1), Numeric::NotNumeric);
        assert_eq!(num.get(v.len() + 5), Numeric::NotNumeric);
        let all: Vec<u64> = (0..v.len() + 3).collect();
        // compared as text: NaN is not equal to itself
        let show = |n: &[Numeric]| format!("{n:?}");
        assert_eq!(
            show(&num.get_many(&all)),
            show(&all.iter().map(|&i| num.get(i)).collect::<Vec<_>>())
        );
    }

    #[test]
    fn a_column_of_another_vocabulary_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for i in 0..40 {
            w.push(&term_key(
                &Literal::new_typed_literal(format!("{i:03}"), oxrdf::vocab::xsd::INTEGER).into(),
            ))
            .unwrap();
        }
        w.finish().unwrap();
        let col = std::fs::read(dir.path().join(FILE)).unwrap();
        assert!(Vocab::open(dir.path()).unwrap().numeric().is_some());
        // a vocabulary without numbers writes no column
        let other = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(other.path()).unwrap();
        w.push(b"\"abc\xff").unwrap();
        w.push(b"<http://ex.org/a").unwrap();
        w.finish().unwrap();
        assert!(!other.path().join(FILE).exists());
        assert!(!other.path().join(format!("{FILE}.tmp")).exists());
        assert!(!other.path().join(format!("{FILE}.values.tmp")).exists());
        // the first vocabulary's column next to the second one
        std::fs::write(other.path().join(FILE), &col).unwrap();
        assert!(Vocab::open(other.path()).unwrap().numeric().is_none());
        // a truncated column
        std::fs::write(dir.path().join(FILE), &col[..col.len() - 8]).unwrap();
        assert!(Vocab::open(dir.path()).unwrap().numeric().is_none());
        // added to a vocabulary built without it
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        assert!(Vocab::open(dir.path()).unwrap().numeric().is_none());
        assert_eq!(crate::vocab::add_numeric_column(dir.path()).unwrap(), 40);
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), col);
        let v = Vocab::open(dir.path()).unwrap();
        assert_eq!(v.numeric().unwrap().get(7), Numeric::Integer(7));
    }
}
