//! Opt-in FastCDC for immutable vocabulary files (F11 §6).

use crate::{BackupError, Result};
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};
use serde::{Deserialize, Serialize};
use sparkles_core::store::{CapturedFile, FileKind};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) const FEATURE: &str = "fastcdc-2016/1";
const MIN: usize = 1 << 20;
const AVG: usize = 4 << 20;
const MAX: usize = 16 << 20;
// The masks spread their 24 and 20 bits across 48 positions so older bytes
// contribute to a cut. The relaxed mask is a subset of the tight mask.
const MASK_SMALL: u64 = 0x0000_a5a5_a5a5_a5a5;
const MASK_LARGE: u64 = 0x0000_2525_a525_a525;

/// The supported per-file chunker. Its parameters are fixed by its version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Chunker {
    FastCdc2016,
    /// Future identifiers survive parsing so validation can refuse them by version.
    Unknown(String),
}

impl From<String> for Chunker {
    fn from(value: String) -> Self {
        if value == FEATURE {
            Self::FastCdc2016
        } else {
            Self::Unknown(value)
        }
    }
}

impl From<Chunker> for String {
    fn from(value: Chunker) -> Self {
        match value {
            Chunker::FastCdc2016 => FEATURE.into(),
            Chunker::Unknown(name) => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunking {
    #[serde(rename = "vocab.dat")]
    pub vocab: Parameters,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    algo: Algorithm,
    min: usize,
    avg: usize,
    max: usize,
    nc: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Algorithm {
    #[serde(rename = "fastcdc-2016")]
    FastCdc2016,
}

pub(crate) const V1: Chunking = Chunking {
    vocab: Parameters {
        algo: Algorithm::FastCdc2016,
        min: MIN,
        avg: AVG,
        max: MAX,
        nc: 2,
    },
};

pub(crate) fn eligible(path: &str, kind: FileKind) -> bool {
    kind == FileKind::Immutable
        && path
            .split_once('/')
            .is_some_and(|(dir, file)| crate::layout::valid_generation(dir) && file == "vocab.dat")
}

pub(crate) fn valid_sizes(blobs: &[crate::BlobRef]) -> bool {
    let Some((last, preceding)) = blobs.split_last() else {
        return false;
    };
    if last.size == 0 {
        return preceding.is_empty();
    }
    (1..=MAX as u64).contains(&last.size)
        && preceding
            .iter()
            .all(|b| (MIN as u64..=MAX as u64).contains(&b.size))
}

struct GearLength;

impl KeyType for GearLength {
    fn len(&self) -> usize {
        256 * 8
    }
}

pub(crate) fn gear(
    repository: Uuid,
    security: &crate::security::Security,
) -> Result<Zeroizing<[u64; 256]>> {
    let mut seed: &[u8] = repository.as_bytes();
    #[cfg(feature = "encryption")]
    if let Some(keys) = &security.sealed {
        seed = keys.active().chunker.as_ref();
    }
    #[cfg(not(feature = "encryption"))]
    let _ = (&mut seed, security);
    let salt = Salt::new(HKDF_SHA256, repository.as_bytes());
    let prk = salt.extract(seed);
    let mut bytes = Zeroizing::new([0u8; 256 * 8]);
    prk.expand(&[b"sparkles/f11/fastcdc-2016/1/gear"], GearLength)
        .and_then(|okm| okm.fill(bytes.as_mut()))
        .map_err(|_| BackupError::internal("cannot derive the vocabulary chunking table"))?;
    let mut table = Zeroizing::new([0u64; 256]);
    let (words, _) = bytes.as_chunks::<8>();
    for (word, bytes) in table.iter_mut().zip(words) {
        *word = u64::from_le_bytes(*bytes);
    }
    Ok(table)
}

/// The rolling state of one vocabulary stream. Both native captures and copy use
/// this state so a source piece boundary has no effect on the destination.
#[derive(Default)]
pub(crate) struct Boundaries {
    hash: u64,
    len: usize,
}

impl Boundaries {
    /// Consume through the next cut, or through the end of `bytes`.
    pub(crate) fn take(&mut self, bytes: &[u8], gear: &[u64; 256]) -> (usize, bool) {
        for (index, &byte) in bytes.iter().enumerate() {
            self.len += 1;
            if self.len < MIN {
                continue;
            }
            self.hash = (self.hash << 1).wrapping_add(gear[byte as usize]);
            let mask = if self.len < AVG {
                MASK_SMALL
            } else {
                MASK_LARGE
            };
            if self.len == MAX || (self.hash & mask) == 0 {
                *self = Self::default();
                return (index + 1, true);
            }
        }
        (bytes.len(), false)
    }
}

pub(crate) fn pieces(
    file: &CapturedFile,
    gear: &[u64; 256],
    cancel: &AtomicBool,
) -> Result<Vec<(u64, u64)>> {
    if file.len == 0 {
        return Ok(vec![(0, 0)]);
    }
    let mut pieces = Vec::new();
    let mut buffer = vec![0; file.len.min(MIN as u64) as usize];
    let mut pos = 0;
    let mut start = 0;
    let mut boundaries = Boundaries::default();
    while pos < file.len {
        if cancel.load(Ordering::Relaxed) {
            return Err(BackupError::cancelled());
        }
        let n = (file.len - pos).min(buffer.len() as u64) as usize;
        file.read_exact_at(pos, &mut buffer[..n])?;
        let mut remaining = &buffer[..n];
        while !remaining.is_empty() {
            let (consumed, cut) = boundaries.take(remaining, gear);
            pos += consumed as u64;
            remaining = &remaining[consumed..];
            if cut {
                pieces.push((start, pos - start));
                start = pos;
            }
        }
    }
    if pos > start {
        pieces.push((start, pos - start));
    }
    Ok(pieces)
}
