//! FSST-compressed suffixes of the base vocabulary (spec X11 §4.1, Phase 2a).
//!
//! FSST (Boncz, Neumann and Leis, PVLDB 13(11), 2020) replaces substrings of up to 8
//! bytes with one-byte codes from a table of at most 255 symbols and decodes every string
//! on its own. A vocabulary written with the `fsst` codec keeps front coding: the first
//! key of each block stays plain, so the sparse index and the binary search over blocks
//! decode nothing, and each other key stores its shared-prefix length and its suffix,
//! which is FSST-coded when that makes it shorter.
//!
//! An entry after the first of its block is `shared` and `len << 1 | coded` as varints,
//! followed by `len` bytes: the suffix itself when `coded` is 0, and its codes when it is
//! 1. A code below 255 stands for its symbol and 255 for the byte that follows it.
//!
//! Two tables serve a vocabulary. Ids below [`Tables::split`] (the literals) use the
//! first and the others (triple terms and IRIs) the second, since the two kinds of key
//! differ in content. The tables are stored in `vocab.fsst`:
//!
//! ```text
//! magic "SPKFSST\0", version u32, reserved u32 (0), split u64,
//! two tables, each: n u8, n symbols of 8 bytes, n lengths u8,
//! checksum u64 (the first 8 bytes of the SHA-256 of everything before it)
//! ```
//!
//! The tables are trained with the `fsst-rs` crate at build time. Reading needs only the
//! tables, and the decoder here is independent of that crate.

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The file that holds the tables, next to `vocab.dat`.
pub const FILE: &str = "vocab.fsst";
const MAGIC: &[u8; 8] = b"SPKFSST\0";
/// The version of `vocab.fsst` this build writes and reads.
pub const VERSION: u32 = 1;
/// The code that marks the next byte as itself.
pub const ESCAPE: u8 = 255;
/// Symbols in a table at most.
const MAX_SYMBOLS: usize = 255;
/// Bytes of suffixes a table is trained on at most, per kind of key. The trainer samples
/// 16 KiB of what it is given.
pub const TRAIN_BYTES: usize = 256 << 10;

/// One symbol table, for decoding.
#[derive(Clone)]
pub struct Table {
    /// the symbols by code, each padded to 8 bytes; the last entry, for the escape
    /// code, stays empty, so that [`Table::decode_at`] can pad a short sequence with it
    syms: [[u8; 8]; MAX_SYMBOLS + 1],
    /// the length of each symbol, 1 to 8, and 0 for a code without a symbol
    lens: [u8; MAX_SYMBOLS + 1],
    n: u8,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            syms: [[0; 8]; MAX_SYMBOLS + 1],
            lens: [0; MAX_SYMBOLS + 1],
            n: 0,
        }
    }
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Table({} symbols)", self.n)
    }
}

/// Cooperative admission and checkpoint hooks for [`Table::measure_with`].
pub(super) trait SuffixMeasure<E> {
    fn admit(&mut self, length: usize) -> Result<(), E>;
    fn checkpoint(&mut self, phase: &'static str) -> Result<(), E>;
    fn overflow(&self) -> E;
}

struct UncheckedMeasure;

impl SuffixMeasure<std::convert::Infallible> for UncheckedMeasure {
    #[inline(always)]
    fn admit(&mut self, _length: usize) -> Result<(), std::convert::Infallible> {
        Ok(())
    }
    #[inline(always)]
    fn checkpoint(&mut self, _phase: &'static str) -> Result<(), std::convert::Infallible> {
        Ok(())
    }
    fn overflow(&self) -> std::convert::Infallible {
        unreachable!("unchecked measure cannot overflow")
    }
}

impl Table {
    pub(crate) fn first_byte_with<C: crate::budgeted::Control>(
        &self,
        codes: &[u8],
        control: &C,
    ) -> Result<Option<u8>> {
        let mut index = 0usize;
        while index < codes.len() {
            if C::CHECKS && index.is_multiple_of(64 << 10) {
                control.check(0)?;
            }
            let code = codes[index];
            if code == ESCAPE {
                return Ok(codes.get(index + 1).copied());
            }
            if self.lens[code as usize] != 0 {
                return Ok(Some(self.syms[code as usize][0]));
            }
            index += 1;
        }
        control.check(0)?;
        Ok(None)
    }

    /// The number of symbols.
    pub fn symbols(&self) -> usize {
        self.n as usize
    }

    /// Append the bytes that `codes` stand for to `out`. Codes without a symbol add
    /// nothing, and an escape at the end adds nothing, so damaged data decodes to a wrong
    /// key and not to a panic (`sparkles check` finds it).
    #[inline]
    pub fn decode_into(&self, codes: &[u8], out: &mut Vec<u8>) {
        // Every code adds at most 8 bytes. With that room reserved, each symbol is stored
        // as one unaligned 8-byte word and the length then advanced by the symbol's own
        // length, which is FSST's decoding loop and several times faster than a copy of
        // a variable length.
        out.reserve(codes.len() * 8);
        let base = out.as_mut_ptr();
        let mut len = out.len();
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            if c == ESCAPE {
                if let Some(&b) = codes.get(i + 1) {
                    // SAFETY: `len` < the capacity reserved above: at most 8 bytes were
                    // written per code before this one, and this code has 8 reserved.
                    unsafe { base.add(len).write(b) };
                    len += 1;
                }
                i += 2;
            } else {
                // SAFETY: as above, `len + 8` is within the reserved capacity, and the
                // write is unaligned.
                unsafe {
                    base.add(len)
                        .cast::<[u8; 8]>()
                        .write_unaligned(self.syms[c as usize])
                };
                len += self.lens[c as usize] as usize;
                i += 1;
            }
        }
        // SAFETY: the bytes up to `len` were written above (or were there before), and
        // `len` is within the capacity.
        unsafe { out.set_len(len) };
    }

    /// Append what `codes` stand for without reserving more than that (see
    /// [`Table::decoded_len`]).
    #[inline]
    pub fn decode_exact(&self, codes: &[u8], out: &mut Vec<u8>) {
        out.reserve_exact(self.decoded_len(codes));
        self.decode_upto(codes, usize::MAX, out);
    }

    /// Append to `out` the first `max` bytes of what `codes` stand for, or all of them
    /// when they are fewer, without growing `out`. A caller reserves the room first:
    /// [`Table::decoded_len`] bytes for a whole suffix, or `max` for a prefix of it, and
    /// a decode stops at the capacity, so damaged codes cut the key short instead of
    /// writing past it.
    ///
    /// Each symbol is written as one unaligned 8-byte store while 8 bytes of capacity
    /// remain, as in [`Table::decode_into`], and copied by its length near the end of the
    /// room. Decoding a prefix lets a front-coded block skip the bytes of a key that no
    /// later key takes, which is most of each key but the one asked for.
    #[inline]
    pub fn decode_upto(&self, codes: &[u8], max: usize, out: &mut Vec<u8>) {
        let cap = out.capacity();
        let limit = out.len().saturating_add(max).min(cap);
        let base = out.as_mut_ptr();
        let mut len = out.len();
        let mut i = 0;
        while i < codes.len() && len < limit {
            let c = codes[i];
            if c == ESCAPE {
                if let Some(&b) = codes.get(i + 1) {
                    // SAFETY: `len` < `limit` <= the capacity.
                    unsafe { base.add(len).write(b) };
                    len += 1;
                }
                i += 2;
            } else {
                let l = self.lens[c as usize] as usize;
                if len + 8 <= cap {
                    // SAFETY: 8 bytes from `len` are within the capacity, and the write
                    // is unaligned.
                    unsafe {
                        base.add(len)
                            .cast::<[u8; 8]>()
                            .write_unaligned(self.syms[c as usize])
                    };
                } else {
                    let n = l.min(cap - len);
                    // SAFETY: `n` bytes from `len` are within the capacity, and the
                    // symbol holds at least `n` bytes.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            self.syms[c as usize].as_ptr(),
                            base.add(len),
                            n,
                        )
                    };
                }
                len += l;
                i += 1;
            }
        }
        // SAFETY: the bytes up to `limit` were written above (or were there before), and
        // `limit` is within the capacity. A symbol that crossed `limit` is cut there.
        unsafe { out.set_len(len.min(limit)) };
    }

    /// [`Table::decode_upto`] after reserving the room, for decodes that do not account
    /// for their buffer.
    #[inline]
    pub fn decode_prefix_into(&self, codes: &[u8], max: usize, out: &mut Vec<u8>) {
        out.reserve(max.min(codes.len() * 8).saturating_add(8));
        self.decode_upto(codes, max, out);
    }

    /// Decode the codes `data[start..start + len]` as [`Table::decode_upto`] does with
    /// `max`, or, with `grow`, as [`Table::decode_into`] and
    /// [`Table::decode_prefix_into`] do after reserving the room.
    ///
    /// Most suffixes of a front-coded vocabulary are two to four codes, so the loop of
    /// the other decoders ends after a varying number of steps and its branch is often
    /// mispredicted. A sequence of at most eight codes without an escape, whose eight
    /// bytes `data` holds, is decoded instead from one 8-byte load in four or eight
    /// fixed steps, with the bytes past the sequence read as the escape code, whose
    /// symbol is empty. This needs 64 bytes of room, and a decode without it takes the
    /// loop.
    #[inline(always)]
    pub fn decode_at(
        &self,
        data: &[u8],
        start: usize,
        len: usize,
        max: usize,
        out: &mut Vec<u8>,
        grow: bool,
    ) {
        if len <= 8
            && let Some(word) = data.get(start..start + 8)
        {
            if grow {
                out.reserve(64);
            }
            if out.capacity() - out.len() >= 64 {
                let w = u64::from_le_bytes(word.try_into().unwrap());
                let valid = if len == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * len)) - 1
                };
                // a zero byte of `x` within the sequence is an escape there
                let x = !w | !valid;
                if x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080 == 0 {
                    let codes = w | !valid;
                    let begin = out.len();
                    let base = out.as_mut_ptr();
                    let mut at = begin;
                    let steps = if len <= 4 { 4 } else { 8 };
                    for k in 0..steps {
                        let c = (codes >> (8 * k)) as u8 as usize;
                        // SAFETY: each step writes 8 bytes and advances by at most 8,
                        // so every write stays within the 64 bytes of room checked
                        // above, and the write is unaligned.
                        unsafe { base.add(at).cast::<[u8; 8]>().write_unaligned(self.syms[c]) };
                        at += self.lens[c] as usize;
                    }
                    // SAFETY: the bytes up to `at` were written above, within the
                    // capacity; a prefix keeps the first `max` of them.
                    unsafe { out.set_len(at.min(begin.saturating_add(max))) };
                    return;
                }
            }
        }
        let codes = &data[start..start + len];
        if !grow {
            self.decode_upto(codes, max, out)
        } else if max == usize::MAX {
            self.decode_into(codes, out)
        } else {
            self.decode_prefix_into(codes, max, out)
        }
    }

    /// Compare the decoded bytes of `codes` against `other`, returning the ordering and
    /// the length of their longest common prefix without allocating.
    ///
    /// Matches [`Table::decode_into`] on damaged input: codes without a symbol and a
    /// trailing escape emit no bytes.
    #[inline]
    pub fn cmp_with_lcp(&self, codes: &[u8], other: &[u8]) -> (std::cmp::Ordering, usize) {
        let mut matched = 0usize;
        let mut i = 0usize;
        while i < codes.len() {
            let c = codes[i];
            if c == ESCAPE {
                if let Some(&b) = codes.get(i + 1) {
                    let Some(&ob) = other.get(matched) else {
                        return (std::cmp::Ordering::Greater, matched);
                    };
                    if b != ob {
                        return (b.cmp(&ob), matched);
                    }
                    matched += 1;
                }
                i += 2;
            } else {
                let l = self.lens[c as usize] as usize;
                if l != 0 {
                    let sym = &self.syms[c as usize][..l];
                    let rem = &other[matched..];
                    let n = l.min(rem.len());
                    if sym[..n] == rem[..n] {
                        matched += n;
                        if l > rem.len() {
                            return (std::cmp::Ordering::Greater, matched);
                        }
                    } else {
                        for k in 0..n {
                            if sym[k] != rem[k] {
                                return (sym[k].cmp(&rem[k]), matched + k);
                            }
                        }
                    }
                }
                i += 1;
            }
        }
        (matched.cmp(&other.len()), matched)
    }

    /// The number of bytes that `codes` decode to.
    #[inline]
    pub fn decoded_len(&self, codes: &[u8]) -> usize {
        self.measure_with::<false, _, _>(codes, &mut UncheckedMeasure)
            .unwrap_or_else(|e| match e {})
    }

    /// Measure the decoded length of `codes`. With `POLLS = false`, compiles to the
    /// unchecked loop; with `POLLS = true`, admits length incrementally and polls
    /// checkpoints every 1024 code bytes.
    #[inline(always)]
    pub(super) fn measure_with<const POLLS: bool, M: SuffixMeasure<E>, E>(
        &self,
        codes: &[u8],
        measure: &mut M,
    ) -> Result<usize, E> {
        if !POLLS {
            let mut n = 0usize;
            let mut i = 0usize;
            while i < codes.len() {
                let c = codes[i];
                if c == ESCAPE {
                    n += usize::from(i + 1 < codes.len());
                    i += 2;
                } else {
                    n += self.lens[c as usize] as usize;
                    i += 1;
                }
            }
            return Ok(n);
        }
        let (mut at, mut length, mut since_check) = (0usize, 0usize, 0usize);
        while at < codes.len() {
            let code = codes[at];
            let (bytes, consumed) = if code == ESCAPE {
                (usize::from(at + 1 < codes.len()), 2)
            } else {
                (self.lens[code as usize] as usize, 1)
            };
            length = length
                .checked_add(bytes)
                .ok_or_else(|| measure.overflow())?;
            at += consumed;
            since_check += consumed;
            if since_check >= 1024 {
                // Admit incrementally, so a low cap refuses before a full long scan.
                measure.admit(length)?;
                measure.checkpoint("measuring vocabulary suffix")?;
                since_check = 0;
            }
        }
        measure.admit(length)?;
        measure.checkpoint("finishing vocabulary measurement")?;
        Ok(length)
    }

    /// Measure the decoded length of `codes` up to `max_bytes`, stopping once at least
    /// `max_bytes` decoded bytes are observed so a giant suffix is not scanned in full
    /// for a short prefix lookup.
    pub(super) fn measure_prefix_controlled<E: From<Error>>(
        &self,
        codes: &[u8],
        max_bytes: usize,
        mut admit: impl FnMut(usize) -> Result<(), E>,
        checkpoint: &mut impl FnMut(&'static str) -> Result<(), E>,
    ) -> Result<usize, E> {
        let (mut at, mut length, mut since_check) = (0usize, 0usize, 0usize);
        while at < codes.len() && length < max_bytes {
            let code = codes[at];
            let (bytes, consumed) = if code == ESCAPE {
                (usize::from(at + 1 < codes.len()), 2)
            } else {
                (self.lens[code as usize] as usize, 1)
            };
            length = length
                .checked_add(bytes)
                .ok_or_else(|| Error::invalid("vocabulary suffix overflows"))?
                .min(max_bytes);
            at += consumed;
            since_check += consumed;
            if since_check >= 1024 {
                admit(length)?;
                checkpoint("measuring vocabulary suffix")?;
                since_check = 0;
            }
        }
        admit(length)?;
        checkpoint("finishing vocabulary measurement")?;
        Ok(length)
    }

    pub(super) fn decode_controlled<E>(
        &self,
        codes: &[u8],
        max: usize,
        output: &mut Vec<u8>,
        checkpoint: &mut impl FnMut(&'static str) -> Result<(), E>,
    ) -> Result<(), E> {
        let limit = output.len().saturating_add(max).min(output.capacity());
        let (mut at, mut since_check) = (0usize, 0usize);
        while at < codes.len() && output.len() < limit {
            let code = codes[at];
            if code == ESCAPE {
                if let Some(&byte) = codes.get(at + 1) {
                    output.push(byte);
                }
                at += 2;
                since_check += 2;
            } else {
                let length = (self.lens[code as usize] as usize).min(limit - output.len());
                output.extend_from_slice(&self.syms[code as usize][..length]);
                at += 1;
                since_check += 1;
            }
            if since_check >= 1024 {
                checkpoint("decoding vocabulary suffix")?;
                since_check = 0;
            }
        }
        checkpoint("finishing vocabulary decode")
    }

    /// Stream-decode `codes` and compare the decoded suffix directly against `target`
    /// without allocating a scratch buffer. Returns the lexicographical ordering and
    /// the number of leading bytes that matched `target`, stopping as soon as a byte
    /// differs or the decoded suffix extends past `target`.
    pub(super) fn compare_controlled<E>(
        &self,
        codes: &[u8],
        target: &[u8],
        checkpoint: &mut impl FnMut(&'static str) -> Result<(), E>,
    ) -> Result<(std::cmp::Ordering, usize), E> {
        let (mut at, mut matched, mut since_check) = (0usize, 0usize, 0usize);
        while at < codes.len() {
            let code = codes[at];
            let (sym, consumed): (&[u8], usize) = if code == ESCAPE {
                if let Some(slice) = codes.get(at + 1..at + 2) {
                    (slice, 2)
                } else {
                    at += 2;
                    continue;
                }
            } else {
                let len = self.lens[code as usize] as usize;
                (&self.syms[code as usize][..len], 1)
            };
            let rem = &target[matched..];
            if rem.is_empty() {
                if !sym.is_empty() {
                    checkpoint("completed vocabulary comparison")?;
                    return Ok((std::cmp::Ordering::Greater, matched));
                }
            } else {
                let n = sym.len().min(rem.len());
                for (i, (&a, &b)) in sym[..n].iter().zip(&rem[..n]).enumerate() {
                    if a != b {
                        checkpoint("completed vocabulary comparison")?;
                        return Ok((a.cmp(&b), matched + i));
                    }
                }
                matched += n;
                if sym.len() > n {
                    checkpoint("completed vocabulary comparison")?;
                    return Ok((std::cmp::Ordering::Greater, matched));
                }
            }
            at += consumed;
            since_check += consumed;
            if since_check >= 1024 {
                checkpoint("comparing vocabulary suffix")?;
                since_check = 0;
            }
        }
        checkpoint("completed vocabulary comparison")?;
        Ok((matched.cmp(&target.len()), matched))
    }

    /// Whether `codes` is a valid code sequence of this table: every code has a symbol
    /// and no escape ends it.
    pub fn valid(&self, codes: &[u8]) -> bool {
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            if c == ESCAPE {
                if i + 1 >= codes.len() {
                    return false;
                }
                i += 2;
            } else {
                if c >= self.n {
                    return false;
                }
                i += 1;
            }
        }
        true
    }

    fn from_parts(syms: &[u64], lens: &[u8]) -> Result<Table> {
        let bad = |m: &str| Error::Corrupt(format!("{FILE}: {m}"));
        if syms.len() != lens.len() || syms.len() > MAX_SYMBOLS {
            return Err(bad("symbol table size"));
        }
        let mut t = Table {
            n: syms.len() as u8,
            ..Table::default()
        };
        for (i, (&s, &l)) in syms.iter().zip(lens).enumerate() {
            if !(1..=8).contains(&l) {
                return Err(bad("symbol length"));
            }
            t.syms[i] = s.to_le_bytes();
            t.lens[i] = l;
        }
        Ok(t)
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.push(self.n);
        for s in &self.syms[..self.n as usize] {
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&self.lens[..self.n as usize]);
    }

    fn read(buf: &[u8], pos: &mut usize) -> Result<Table> {
        let bad = || Error::Corrupt(format!("{FILE}: cut short"));
        let n = *buf.get(*pos).ok_or_else(bad)? as usize;
        *pos += 1;
        let syms_end = *pos + n * 8;
        let lens_end = syms_end + n;
        let syms: Vec<u64> = buf
            .get(*pos..syms_end)
            .ok_or_else(bad)?
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        let lens = buf.get(syms_end..lens_end).ok_or_else(bad)?;
        *pos = lens_end;
        Table::from_parts(&syms, lens)
    }
}

/// The two tables of a vocabulary.
#[derive(Clone, Debug, Default)]
pub struct Tables {
    /// the first id that uses the second table
    pub split: u64,
    pub tables: [Table; 2],
}

impl Tables {
    /// The table of the key with id `id`.
    #[inline(always)]
    pub fn table(&self, id: u64) -> &Table {
        &self.tables[usize::from(id >= self.split)]
    }

    /// The bytes of `vocab.fsst`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + 2 * 9 * MAX_SYMBOLS);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&self.split.to_le_bytes());
        for t in &self.tables {
            t.write(&mut out);
        }
        let sum = checksum(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// Parse `vocab.fsst`. Another version is refused with the format registry's message.
    pub fn decode(buf: &[u8]) -> Result<Tables> {
        let bad = |m: &str| Error::Corrupt(format!("{FILE}: {m}"));
        if buf.get(..8) != Some(MAGIC) {
            return Err(bad("not a table file"));
        }
        let u32_at = |i: usize| {
            buf.get(i..i + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let version = u32_at(8).ok_or_else(|| bad("cut short"))?;
        crate::formats::VOCAB_FSST.check(FILE, version, None)?;
        if buf.len() < 8 + 24 {
            return Err(bad("cut short"));
        }
        let (body, sum) = buf.split_at(buf.len() - 8);
        if checksum(body) != u64::from_le_bytes(sum.try_into().unwrap()) {
            return Err(bad("checksum mismatch"));
        }
        let split = u64::from_le_bytes(body[16..24].try_into().unwrap());
        let mut pos = 24;
        let a = Table::read(body, &mut pos)?;
        let b = Table::read(body, &mut pos)?;
        if pos != body.len() {
            return Err(bad("trailing bytes"));
        }
        Ok(Tables {
            split,
            tables: [a, b],
        })
    }

    /// Read `vocab.fsst` in `dir`.
    pub fn read(dir: &Path) -> Result<Tables> {
        let path = dir.join(FILE);
        let buf = std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Corrupt(format!(
                    "{}: missing, and meta.json says the vocabulary is FSST-coded",
                    path.display()
                ))
            } else {
                e.into()
            }
        })?;
        Tables::decode(&buf)
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        crate::store::write_synced(&dir.join(FILE), &self.encode())
    }
}

/// The version of the table file at `path`, read from its header alone.
pub fn file_version(path: &Path) -> Result<u32> {
    use std::io::Read;
    let mut head = [0u8; 12];
    std::fs::File::open(path)?
        .read_exact(&mut head)
        .map_err(|_| Error::Corrupt(format!("{}: cut short", path.display())))?;
    if &head[..8] != MAGIC {
        return Err(Error::Corrupt(format!(
            "{}: not a table file",
            path.display()
        )));
    }
    Ok(u32::from_le_bytes(head[8..12].try_into().unwrap()))
}

fn checksum(b: &[u8]) -> u64 {
    let h = Sha256::digest(b);
    u64::from_le_bytes(h[..8].try_into().unwrap())
}

/// Whether `key` uses the first table: literal keys (`"…`) sort before triple terms
/// (`(…`) and IRIs (`<…`).
#[inline]
pub fn is_first_kind(key: &[u8]) -> bool {
    key.first().is_some_and(|&b| b < b'(')
}

/// The encoders of the two tables, for a build.
pub struct Encoder {
    comp: [fsst::Compressor; 2],
    tables: [Table; 2],
}

impl Encoder {
    /// Train the two tables on sample suffixes of each kind of key. An empty sample gives
    /// a table without symbols, whose suffixes are all stored plain.
    pub fn train(samples: [&[&[u8]]; 2]) -> Encoder {
        let comp = samples.map(|s| {
            let s: Vec<&[u8]> = s.iter().copied().filter(|x| !x.is_empty()).collect();
            if s.is_empty() {
                fsst::Compressor::rebuild_from(Vec::<fsst::Symbol>::new(), Vec::<u8>::new())
            } else {
                fsst::Compressor::train(&s)
            }
        });
        let tables = [0, 1].map(|i| {
            let c = &comp[i];
            let syms: Vec<u64> = c.symbol_table().iter().map(|s| s.to_u64()).collect();
            // the trainer gives lengths of 1 to 8 for its symbols
            Table::from_parts(&syms, c.symbol_lengths()).expect("a trained FSST table")
        });
        Encoder { comp, tables }
    }

    /// The tables, with `split` the first id of the second kind.
    pub fn tables(&self, split: u64) -> Tables {
        Tables {
            split,
            tables: self.tables.clone(),
        }
    }

    /// Code `s` with table `which` into `out` (cleared first). False, and `out` empty,
    /// when the codes would not be shorter than `s`, which is then stored plain.
    pub fn encode(&self, which: usize, s: &[u8], out: &mut Vec<u8>) -> bool {
        out.clear();
        if s.is_empty() || self.tables[which].n == 0 {
            return false;
        }
        let c = self.comp[which].compress(s);
        if c.len() >= s.len() {
            return false;
        }
        out.extend_from_slice(&c);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(enc: &Encoder, which: usize, s: &[u8]) {
        let mut c = Vec::new();
        let coded = enc.encode(which, s, &mut c);
        let t = &enc.tables[which];
        if coded {
            assert!(c.len() < s.len());
            assert!(t.valid(&c), "{s:?}");
            assert_eq!(t.decoded_len(&c), s.len());
            let mut out = b"pre".to_vec();
            t.decode_into(&c, &mut out);
            assert_eq!(&out[3..], s);
            let mut out = Vec::new();
            t.decode_exact(&c, &mut out);
            assert_eq!(out, s);
            // the branch-free path reads past the codes, which must not change them;
            // without bytes past them, and without room, the loop decodes
            for tail in [&[][..], &[0u8; 8][..], &[255u8; 8][..]] {
                let wide = [b"xy".as_slice(), &c, tail].concat();
                for max in [0, 1, 3, 7, 8, 9, s.len(), usize::MAX] {
                    let want = &s[..s.len().min(max)];
                    let mut out = b"pre".to_vec();
                    t.decode_at(&wide, 2, c.len(), max, &mut out, true);
                    assert_eq!(&out[3..], want, "{s:?} max {max}");
                    let mut out = Vec::with_capacity(want.len());
                    t.decode_at(&wide, 2, c.len(), max, &mut out, false);
                    assert_eq!(out, want);
                    let mut out = Vec::with_capacity(want.len() + 64);
                    t.decode_at(&wide, 2, c.len(), max, &mut out, false);
                    assert_eq!(out, want);
                }
            }
        } else {
            assert!(c.is_empty());
        }
    }

    fn corpus() -> Vec<Vec<u8>> {
        let mut v: Vec<Vec<u8>> = (0..2000)
            .map(|i| {
                format!("\"label number {i} of the http://example.org/thing\"@en").into_bytes()
            })
            .collect();
        // short pieces, which code to eight codes or fewer
        for i in 0..200 {
            let l = &v[i];
            v.push(l[i % 20..i % 20 + 1 + i % 23].to_vec());
        }
        v.push(Vec::new());
        v.push((0..=255u8).collect());
        v.push(vec![255; 300]);
        v.push(vec![0; 40]);
        v.push(b"x".repeat(100_000));
        v
    }

    #[test]
    fn round_trip_of_random_and_adversarial_strings() {
        let c = corpus();
        let refs: Vec<&[u8]> = c.iter().map(|v| &v[..]).collect();
        let enc = Encoder::train([&refs, &refs[..10]]);
        assert!(enc.tables[0].symbols() > 10);
        for s in &c {
            roundtrip(&enc, 0, s);
            roundtrip(&enc, 1, s);
        }
        // random bytes, with a seeded generator
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for len in 0..300 {
            let s: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            roundtrip(&enc, 0, &s);
        }
    }

    #[test]
    fn cmp_with_lcp_matches_decoded_comparison_without_allocating() {
        let c = corpus();
        let refs: Vec<&[u8]> = c.iter().map(|v| &v[..]).collect();
        let enc = Encoder::train([&refs, &refs[..10]]);
        let t = &enc.tables[0];
        let lcp = |a: &[u8], b: &[u8]| a.iter().zip(b).take_while(|(x, y)| x == y).count();

        let mut coded = Vec::new();
        for s in c.iter().take(64).chain([c.last().unwrap()]) {
            if !enc.encode(0, s, &mut coded) {
                continue;
            }
            let mut probes: Vec<Vec<u8>> = vec![Vec::new(), s.clone()];
            if !s.is_empty() {
                probes.push(s[..s.len() - 1].to_vec());
                probes.push(s[..s.len() / 2].to_vec());
                let mut shorter_diff = s.clone();
                let mid = s.len() / 2;
                shorter_diff[mid] = shorter_diff[mid].wrapping_add(1);
                probes.push(shorter_diff);
                let mut last_diff = s.clone();
                let last = s.len() - 1;
                last_diff[last] = last_diff[last].wrapping_sub(1);
                probes.push(last_diff);
            }
            let mut longer = s.clone();
            longer.push(0);
            probes.push(longer);

            for probe in &probes {
                let (got, stats) =
                    crate::test_alloc::record_alloc_stats(|| t.cmp_with_lcp(&coded, probe));
                assert_eq!(got, (s.as_slice().cmp(probe.as_slice()), lcp(s, probe)));
                assert_eq!(stats.total_alloc, 0, "cmp_with_lcp must not allocate");
            }
        }

        // Damaged inputs (trailing escape, unused code with lens == 0) match decode_into.
        let mut damaged = Vec::new();
        assert!(enc.encode(
            0,
            b"label number 1 of the http://example.org/thing",
            &mut damaged
        ));
        if t.n < MAX_SYMBOLS as u8 {
            damaged.push(t.n);
        }
        damaged.push(ESCAPE);
        let mut decoded = Vec::new();
        t.decode_into(&damaged, &mut decoded);
        for probe in [
            decoded.as_slice(),
            &decoded[..decoded.len().saturating_sub(1)],
            [decoded.as_slice(), b"z"].concat().as_slice(),
        ] {
            let (got, stats) =
                crate::test_alloc::record_alloc_stats(|| t.cmp_with_lcp(&damaged, probe));
            assert_eq!(got, (decoded.as_slice().cmp(probe), lcp(&decoded, probe)));
            assert_eq!(stats.total_alloc, 0);
        }
    }

    #[test]
    fn an_empty_sample_stores_everything_plain() {
        let enc = Encoder::train([&[], &[]]);
        let mut c = Vec::new();
        assert!(!enc.encode(0, b"anything at all", &mut c));
        assert_eq!(enc.tables(0).tables[0].symbols(), 0);
    }

    #[test]
    fn the_file_round_trips_and_refuses_damage_and_other_versions() {
        let c = corpus();
        let refs: Vec<&[u8]> = c.iter().map(|v| &v[..]).collect();
        let enc = Encoder::train([&refs, &refs]);
        let t = enc.tables(1234);
        let bytes = t.encode();
        let back = Tables::decode(&bytes).unwrap();
        assert_eq!(back.split, 1234);
        for i in 0..2 {
            assert_eq!(back.tables[i].n, t.tables[i].n);
            assert_eq!(back.tables[i].syms, t.tables[i].syms);
            assert_eq!(back.tables[i].lens, t.tables[i].lens);
        }
        assert!(std::ptr::eq(back.table(1233), &back.tables[0]));
        assert!(std::ptr::eq(back.table(1234), &back.tables[1]));
        // a flipped byte fails the checksum
        let mut bad = bytes.clone();
        bad[30] ^= 1;
        assert!(matches!(Tables::decode(&bad), Err(Error::Corrupt(_))));
        // a cut file and a wrong magic number
        assert!(Tables::decode(&bytes[..20]).is_err());
        assert!(Tables::decode(b"SPKFSSTX\x01\0\0\0").is_err());
        // a newer version is refused by name
        let mut newer = bytes.clone();
        newer[8..12].copy_from_slice(&2u32.to_le_bytes());
        let e = Tables::decode(&newer).unwrap_err().to_string();
        assert!(e.contains("vocab-fsst") && e.contains("version 2"), "{e}");
    }
}
