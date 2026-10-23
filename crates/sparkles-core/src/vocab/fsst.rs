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
    /// the symbols by code, each padded to 8 bytes
    syms: [[u8; 8]; MAX_SYMBOLS],
    /// the length of each symbol, 1 to 8, and 0 for a code without a symbol
    lens: [u8; MAX_SYMBOLS],
    n: u8,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            syms: [[0; 8]; MAX_SYMBOLS],
            lens: [0; MAX_SYMBOLS],
            n: 0,
        }
    }
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Table({} symbols)", self.n)
    }
}

impl Table {
    /// The number of symbols.
    pub fn symbols(&self) -> usize {
        self.n as usize
    }

    /// Append the bytes that `codes` stand for to `out`. Codes without a symbol add
    /// nothing, and an escape at the end adds nothing, so damaged data decodes to a wrong
    /// key and not to a panic (`sparkles check` finds it).
    #[inline]
    pub fn decode_into(&self, codes: &[u8], out: &mut Vec<u8>) {
        // every code adds at most 8 bytes; with the room reserved, each symbol is copied
        // as 8 bytes and the length then cut back, which is cheaper than a copy of a
        // variable length
        out.reserve(codes.len() * 8);
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            if c == ESCAPE {
                if let Some(&b) = codes.get(i + 1) {
                    out.push(b);
                }
                i += 2;
            } else {
                let at = out.len();
                out.extend_from_slice(&self.syms[c as usize]);
                out.truncate(at + self.lens[c as usize] as usize);
                i += 1;
            }
        }
    }

    /// Append what `codes` stand for without reserving more than that, for the decodes
    /// whose callers account for every byte of their buffer (see
    /// [`Table::decoded_len`]).
    #[inline]
    pub fn decode_exact(&self, codes: &[u8], out: &mut Vec<u8>) {
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            if c == ESCAPE {
                if let Some(&b) = codes.get(i + 1) {
                    out.push(b);
                }
                i += 2;
            } else {
                let l = self.lens[c as usize] as usize;
                out.extend_from_slice(&self.syms[c as usize][..l]);
                i += 1;
            }
        }
    }

    /// The number of bytes that `codes` decode to.
    pub fn decoded_len(&self, codes: &[u8]) -> usize {
        let mut n = 0;
        let mut i = 0;
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
        n
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
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
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
