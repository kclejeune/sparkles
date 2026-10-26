//! Allocation and control checks for read-only cold historical decoding.
//!
//! This module never opens a writer, repairs a file, or grants managed authority.
use crate::{
    error::{Budget, BudgetKind, Error, Result},
    history::ReadLimit,
};
use serde::de::DeserializeOwned;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

mod linked;
pub(super) use linked::read_link_controlled;

const CHUNK: usize = 64 << 10;
// The input stays live while Serde owns its scratch and decoded containers. Metadata
// has no compressed payloads. This covers sparse JSON nodes, vector relocation,
// decoded strings and conversion from Value into the closed metadata structures.
const JSON_PARSE_BYTES_PER_INPUT: u64 = 128;
#[cfg(test)]
type KeyObserver<'a> = &'a dyn Fn(&str, usize);

pub(crate) struct ColdBudget<'a> {
    limit: ReadLimit<'a>,
    used: u64,
    peak: u64,
    #[cfg(test)]
    key_observer: Option<KeyObserver<'a>>,
}

pub(crate) struct Loaded<T> {
    pub(crate) value: T,
    pub(crate) charge: u64,
}

impl<'a> ColdBudget<'a> {
    pub(crate) fn new(limit: ReadLimit<'a>) -> Result<Self> {
        limit.check(0)?;
        Ok(Self {
            limit,
            used: 0,
            peak: 0,
            #[cfg(test)]
            key_observer: None,
        })
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.limit.check(self.used)
    }

    pub(crate) fn key_step(&self, _stage: &str, _at: usize) -> Result<()> {
        #[cfg(test)]
        if let Some(observer) = self.key_observer {
            observer(_stage, _at);
        }
        self.check()
    }

    #[cfg(test)]
    pub(crate) fn observe_keys(&mut self, observer: KeyObserver<'a>) {
        self.key_observer = Some(observer);
    }

    pub(crate) fn used(&self) -> u64 {
        self.used
    }

    fn owned_limit(&self) -> Result<ReadLimit<'a>> {
        self.limit.holding(self.used)
    }

    fn sha256(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        for chunk in bytes.chunks(CHUNK) {
            self.check()?;
            digest.update(chunk);
        }
        self.check()?;
        Ok(digest.finalize().into())
    }

    pub(crate) fn charge(&mut self, bytes: u64) -> Result<()> {
        let next = self
            .used
            .checked_add(bytes)
            .ok_or_else(|| self.exhausted())?;
        self.limit.check(next)?;
        self.used = next;
        self.peak = self.peak.max(next);
        Ok(())
    }

    pub(crate) fn release(&mut self, bytes: u64) {
        assert!(bytes <= self.used, "cold decoder released unowned storage");
        self.used -= bytes;
    }

    pub(crate) fn exhausted(&self) -> Error {
        let limit = self.limit.memory.unwrap_or(u64::MAX);
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            limit,
            requested: limit.saturating_add(1),
        })
    }

    /// Account for old and replacement storage at the same time before a vector
    /// grows. Only vectors admitted through this method may be passed here.
    pub(crate) fn reserve<T>(&mut self, value: &mut Vec<T>, need: usize) -> Result<()> {
        self.check()?;
        if need <= value.capacity() {
            return Ok(());
        }
        let width = std::mem::size_of::<T>() as u64;
        let old = (value.capacity() as u64)
            .checked_mul(width)
            .ok_or_else(|| self.exhausted())?;
        let new_capacity = need.max(value.capacity().saturating_mul(2));
        let new = (new_capacity as u64)
            .checked_mul(width)
            .ok_or_else(|| self.exhausted())?;
        self.charge(new)?;
        if value.try_reserve_exact(new_capacity - value.len()).is_err() {
            self.release(new);
            return Err(self.exhausted());
        }
        self.release(old);
        Ok(())
    }

    pub(crate) fn drop_vec<T>(&mut self, value: Vec<T>) {
        let bytes = value.capacity() as u64 * std::mem::size_of::<T>() as u64;
        drop(value);
        self.release(bytes);
    }

    pub(crate) fn filled<T: Default>(&mut self, count: usize) -> Result<Box<[T]>> {
        let mut values = Vec::new();
        self.reserve(&mut values, count)?;
        for index in 0..count {
            if index.is_multiple_of(1024) {
                self.check()?;
            }
            values.push(T::default());
        }
        self.check()?;
        Ok(values.into_boxed_slice())
    }

    /// Read only the selected immutable byte range. A concurrent truncation is an
    /// error, never an invitation to recover or select another state.
    pub(crate) fn read_range(&mut self, path: &Path, offset: u64, length: u64) -> Result<Vec<u8>> {
        self.check()?;
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        self.read_from(&mut file, length)
    }

    pub(crate) fn read_from<R: Read>(&mut self, file: &mut R, length: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.append_read(file, &mut bytes, length)?;
        Ok(bytes)
    }

    pub(crate) fn append_read<R: Read>(
        &mut self,
        file: &mut R,
        bytes: &mut Vec<u8>,
        length: u64,
    ) -> Result<()> {
        self.check()?;
        let length = usize::try_from(length).map_err(|_| self.exhausted())?;
        let length = bytes
            .len()
            .checked_add(length)
            .ok_or_else(|| self.exhausted())?;
        self.reserve(bytes, length)?;
        // The allocation was admitted before initialization. Chunking keeps both
        // initialization and file reads interruptible without a second eager buffer.
        while bytes.len() < length {
            self.check()?;
            let end = bytes.len().saturating_add(CHUNK).min(length);
            let start = bytes.len();
            bytes.resize(end, 0);
            file.read_exact(&mut bytes[start..end])?;
        }
        self.check()
    }

    pub(crate) fn read_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.check()?;
        let length = std::fs::metadata(path)?.len();
        self.read_range(path, 0, length)
    }

    /// Deserialize through a controlled reader, so cancellation also reaches Serde's
    /// string, escape, object and sequence loops. The raw input is already admitted.
    pub(crate) fn json<T: DeserializeOwned>(&mut self, bytes: &[u8]) -> Result<Loaded<T>> {
        let charge = (bytes.len() as u64)
            .checked_mul(JSON_PARSE_BYTES_PER_INPUT)
            .and_then(|n| n.checked_add(1024))
            .ok_or_else(|| self.exhausted())?;
        self.charge(charge)?;
        let mut reader = ControlledRead {
            bytes,
            until_check: 0,
            limit: self.limit,
            held: self.used,
            failure: None,
        };
        let result = serde_json::from_reader(&mut reader);
        if let Some(error) = reader.failure {
            return Err(error);
        }
        let value =
            result.map_err(|error| Error::Corrupt(format!("historical metadata: {error}")))?;
        self.check()?;
        Ok(Loaded { value, charge })
    }

    /// Check checksums in bounded chunks while their admitted input remains live.
    pub(crate) fn crc32(&self, parts: &[&[u8]]) -> Result<u32> {
        let mut crc = flate2::Crc::new();
        for part in parts {
            for chunk in part.chunks(CHUNK) {
                self.check()?;
                crc.update(chunk);
            }
        }
        self.check()?;
        Ok(crc.sum())
    }
}

struct ControlledRead<'a> {
    bytes: &'a [u8],
    until_check: usize,
    limit: ReadLimit<'a>,
    held: u64,
    failure: Option<Error>,
}

impl Read for ControlledRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.until_check == 0 {
            if let Err(error) = self.limit.check(self.held) {
                self.failure = Some(error);
                return Err(std::io::Error::other("historical metadata interrupted"));
            }
            self.until_check = CHUNK;
        }
        let length = output.len().min(self.bytes.len()).min(self.until_check);
        output[..length].copy_from_slice(&self.bytes[..length]);
        self.bytes = &self.bytes[length..];
        self.until_check -= length;
        Ok(length)
    }
}

impl super::Generation {
    /// Also accounts for a sealed generation first opened by the compatible
    /// unlimited API. Current live generations are handled as store-owned backing
    /// by the caller, rather than copied into each request's owned parse scope.
    pub(crate) fn retained_bytes_limited(&self, limit: &ReadLimit<'_>) -> Result<u64> {
        if self.cold_bytes != 0 {
            limit.check(self.cold_bytes)?;
            return Ok(self.cold_bytes);
        }
        let mut budget = ColdBudget::new(*limit)?;
        budget.charge(
            std::mem::size_of::<Self>() as u64
                + 256
                + self.name.capacity() as u64
                + self.dir.as_ref().map_or(0, |p| p.as_os_str().len()) as u64,
        )?;
        budget.charge(self.vocab.resident_bytes() + 64)?;
        budget.charge(self.dvocab.resident_bytes_limited(limit)?)?;
        budget.charge(
            self.perms.capacity() as u64 * std::mem::size_of::<crate::index::PermIndex>() as u64,
        )?;
        for perm in &self.perms {
            budget.check()?;
            budget.charge(perm.resident_bytes())?;
        }
        let meta = &self.meta;
        budget.charge(
            meta.created.capacity() as u64
                + meta.features.capacity() as u64 * std::mem::size_of::<String>() as u64
                + 256,
        )?;
        for (key, value) in &meta.prefixes {
            budget.check()?;
            budget.charge(key.capacity() as u64 + value.capacity() as u64 + 256)?;
        }
        for feature in &meta.features {
            budget.check()?;
            budget.charge(feature.capacity() as u64)?;
        }
        if let Some(why) = meta.patterns.as_ref().and_then(|p| p.skipped.as_ref()) {
            budget.charge(why.capacity() as u64)?;
        }
        let stats = &self.stats;
        budget.charge(
            stats.predicates.capacity() as u64
                * std::mem::size_of::<crate::builder::PredicateStat>() as u64
                + (stats.graphs.capacity() + stats.classes.capacity()) as u64 * 16
                + stats.charsets.capacity() as u64
                    * std::mem::size_of::<crate::builder::CharSet>() as u64,
        )?;
        for set in &stats.charsets {
            budget.check()?;
            budget.charge((set.preds.capacity() as u64 + set.triples.capacity() as u64) * 8)?;
        }
        budget.charge(if self.patterns.file.is_some() {
            std::mem::size_of::<crate::patterns::PatternFile>() as u64 + 64
        } else {
            self.patterns
                .missing
                .as_ref()
                .map_or(0, |s| s.len() as u64 + 64)
        })?;
        if let Some(link) = &self.link {
            budget.charge(
                std::mem::size_of::<super::link::Linked>() as u64
                    + 64
                    + link.file.segments.capacity() as u64
                        * std::mem::size_of::<super::link::Segment>() as u64
                    + link.base_dir.as_os_str().len() as u64
                    + super::history_limited::preparation_delta_bytes(&link.base_delta),
            )?;
            for segment in &link.file.segments {
                budget.check()?;
                budget.charge(
                    segment.generation.capacity() as u64 + segment.path.capacity() as u64,
                )?;
            }
            if let Some(overlay) = &link.file.overlay {
                budget.charge(overlay.sha256.capacity() as u64)?;
            }
        }
        Ok(budget.used())
    }

    /// Open immutable base files without a writer, recovery or mutable sidecars.
    /// The caller keeps its history/origin retention guard until publication.
    pub(crate) fn open_sealed_controlled(
        dir: &Path,
        name: &str,
        written_by: Option<&str>,
        budget: &mut ColdBudget<'_>,
    ) -> Result<Loaded<Self>> {
        Self::open_base_controlled(dir, name, written_by, None, budget)
    }

    fn open_base_controlled(
        dir: &Path,
        name: &str,
        written_by: Option<&str>,
        dvocab: Option<Loaded<crate::vocab::DeltaVocab>>,
        budget: &mut ColdBudget<'_>,
    ) -> Result<Loaded<Self>> {
        use crate::{
            builder,
            index::Perm,
            vocab::{DeltaVocab, Vocab},
        };
        use std::sync::Arc;
        let start = budget
            .used()
            .checked_sub(dvocab.as_ref().map_or(0, |v| v.charge))
            .expect("supplied dictionary is admitted by the same cold budget");
        // Paths/names and wrapper allocations remain live with the generation.
        budget.charge(
            std::mem::size_of::<Self>() as u64
                + 256
                + name.len() as u64
                + dir.as_os_str().len() as u64,
        )?;
        let bytes = budget.read_file(&dir.join("meta.json"))?;
        // Preserve the existing version-first refusal, including a future metadata
        // shape. Both passes use the same controlled Serde reader.
        let version: Loaded<serde_json::Value> = budget.json(&bytes)?;
        let number = version
            .value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                Error::Corrupt("meta.json: format_version is missing or not a u32".into())
            })?;
        crate::formats::INDEX.check("historical generation", number, written_by)?;
        drop(version.value);
        budget.release(version.charge);
        let meta: Loaded<builder::IndexMeta> = budget.json(&bytes)?;
        budget.drop_vec(bytes);
        // check_features only allocates on an unsupported-feature error. Its
        // payloads and output remain covered by the admitted metadata parse lease.
        for feature in &meta.value.features {
            budget.check()?;
            builder::check_features(
                "historical generation",
                std::slice::from_ref(feature),
                written_by,
            )?;
        }
        let meta = retain_meta(meta, budget)?;
        let bytes = budget.read_file(&dir.join("stats.json"))?;
        let stats: Loaded<builder::Stats> = budget.json(&bytes)?;
        budget.drop_vec(bytes);
        let stats = retain_stats(stats, budget)?;
        let vocab =
            Vocab::open_controlled(dir, meta.has_feature(builder::FEATURE_FSST_VOCAB), budget)?;
        budget.charge(64)?;
        let vocab = Arc::new(vocab.value);
        let dvocab = match dvocab {
            Some(vocab) => vocab,
            None => DeltaVocab::open_read_only_controlled(&dir.join("delta.vocab"), budget)?,
        };
        let mut perms = Vec::new();
        budget.reserve(&mut perms, Perm::ALL.len())?;
        for perm in Perm::ALL {
            budget.check()?;
            perms.push(
                crate::index::PermIndex::open_controlled(dir, perm, meta.format_version, budget)?
                    .value,
            );
        }
        // Pattern opening maps immutable data and decodes at most two fixed table
        // descriptors. Bound its wrappers and optional diagnostic before opening.
        let allowance = std::mem::size_of::<crate::patterns::PatternFile>() as u64
            + 2048
            + dir.as_os_str().len() as u64 * 3
            + meta
                .patterns
                .as_ref()
                .and_then(|p| p.skipped.as_ref())
                .map_or(0, |s| s.len()) as u64;
        budget.charge(allowance)?;
        budget.check()?;
        let patterns = crate::patterns::Loaded::open(dir, meta.patterns.as_ref());
        budget.check()?;
        let retained = if patterns.file.is_some() {
            std::mem::size_of::<crate::patterns::PatternFile>() as u64 + 64
        } else {
            patterns.missing.as_ref().map_or(0, |s| s.len() as u64 + 64)
        };
        budget.release(allowance - retained);
        let charge = budget.used() - start;
        let value = Self {
            uid: crate::index::next_uid(),
            name: name.to_owned(),
            dir: Some(dir.to_owned()),
            vocab,
            perms,
            stats,
            meta,
            dvocab: dvocab.value,
            _tmp: None,
            _base: None,
            vectors: Default::default(),
            geo: Default::default(),
            counts: Default::default(),
            charsets: Default::default(),
            patterns,
            ill_typed_literals: parking_lot::Mutex::new(None),
            wal_index: parking_lot::Mutex::new(None),
            link: None,
            cold_bytes: charge,
        };
        budget.check()?;
        Ok(Loaded { value, charge })
    }
}

fn retain_meta(
    loaded: Loaded<crate::builder::IndexMeta>,
    budget: &mut ColdBudget<'_>,
) -> Result<crate::builder::IndexMeta> {
    let meta = &loaded.value;
    let mut retained = meta.created.capacity() as u64
        + meta.features.capacity() as u64 * std::mem::size_of::<String>() as u64
        + 256;
    for (key, value) in &meta.prefixes {
        budget.check()?;
        retained = retained
            .checked_add(key.capacity() as u64 + value.capacity() as u64 + 256)
            .ok_or_else(|| budget.exhausted())?;
    }
    for feature in &meta.features {
        budget.check()?;
        retained = retained
            .checked_add(feature.capacity() as u64)
            .ok_or_else(|| budget.exhausted())?;
    }
    if let Some(why) = meta.patterns.as_ref().and_then(|p| p.skipped.as_ref()) {
        retained = retained
            .checked_add(why.capacity() as u64)
            .ok_or_else(|| budget.exhausted())?;
    }
    retain_parse(loaded, retained, budget)
}

fn retain_stats(
    loaded: Loaded<crate::builder::Stats>,
    budget: &mut ColdBudget<'_>,
) -> Result<crate::builder::Stats> {
    let stats = &loaded.value;
    let mut retained = stats.predicates.capacity() as u64
        * std::mem::size_of::<crate::builder::PredicateStat>() as u64
        + (stats.graphs.capacity() + stats.classes.capacity()) as u64 * 16
        + stats.charsets.capacity() as u64 * std::mem::size_of::<crate::builder::CharSet>() as u64;
    for set in &stats.charsets {
        budget.check()?;
        retained = retained
            .checked_add((set.preds.capacity() as u64 + set.triples.capacity() as u64) * 8)
            .ok_or_else(|| budget.exhausted())?;
    }
    retain_parse(loaded, retained, budget)
}

fn retain_parse<T>(loaded: Loaded<T>, retained: u64, budget: &mut ColdBudget<'_>) -> Result<T> {
    if retained > loaded.charge {
        budget.charge(retained - loaded.charge)?;
    } else {
        budget.release(loaded.charge - retained);
    }
    Ok(loaded.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryOptions;
    use std::sync::{Arc, atomic::AtomicBool};

    fn immutable_base(fsst: bool) -> tempfile::TempDir {
        use crate::{
            builder::{BuildOptions, Builder, VocabCodec},
            io::{RdfFormat, Source},
        };
        let dir = tempfile::tempdir().unwrap();
        let builder = Builder::new(
            dir.path(),
            BuildOptions {
                threads: 1,
                vocab_codec: Some(if fsst {
                    VocabCodec::Fsst
                } else {
                    VocabCodec::Front
                }),
                ..Default::default()
            },
        )
        .unwrap();
        builder
            .add_source(&Source::from_bytes(
                b"@prefix ex: <urn:ex:> . ex:a ex:p 1, 2, \"value\" ; a ex:Type . ex:b ex:p ex:a ."
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            ))
            .unwrap();
        builder.finish().unwrap();
        dir
    }

    fn durable_bytes(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().into(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn controlled_generation_matches_sealed_base_without_durable_writes() {
        for fsst in [false, true] {
            let dir = immutable_base(fsst);
            let before = durable_bytes(dir.path());
            let normal = super::super::Generation::open_sealed(dir.path(), "gen-0001").unwrap();
            let options = HistoryOptions::default();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let loaded = super::super::Generation::open_sealed_controlled(
                dir.path(),
                "gen-0001",
                None,
                &mut budget,
            )
            .unwrap();
            assert_eq!(loaded.charge, loaded.value.cold_bytes);
            assert_eq!(loaded.charge, budget.used());
            assert_eq!(loaded.value.meta.quads, normal.meta.quads);
            assert_eq!(loaded.value.vocab.len(), normal.vocab.len());
            assert_eq!(loaded.value.stats.charsets, normal.stats.charsets);
            assert_eq!(
                loaded.value.patterns.file.is_some(),
                normal.patterns.file.is_some()
            );
            for perm in crate::index::Perm::ALL {
                assert_eq!(loaded.value.perm(perm).rows, normal.perm(perm).rows);
            }
            for id in 0..normal.vocab.len() {
                assert_eq!(loaded.value.vocab.get(id), normal.vocab.get(id));
            }
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 0);
            assert_eq!(before, durable_bytes(dir.path()));
        }
    }

    #[test]
    fn controlled_generation_refusals_preserve_bytes_and_unknown_version_precedence() {
        let dir = immutable_base(false);
        let options = HistoryOptions::default();
        let before = durable_bytes(dir.path());
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1024),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            super::super::Generation::open_sealed_controlled(
                dir.path(),
                "gen-0001",
                None,
                &mut budget
            ),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(before, durable_bytes(dir.path()));
        std::fs::write(
            dir.path().join("meta.json"),
            br#"{"format_version":4294967295,"future_shape":["different"]}"#,
        )
        .unwrap();
        let before = durable_bytes(dir.path());
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            super::super::Generation::open_sealed_controlled(
                dir.path(),
                "gen-0001",
                None,
                &mut budget
            ),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(before, durable_bytes(dir.path()));
    }

    #[test]
    fn replacement_storage_is_admitted_before_vector_growth() {
        let options = HistoryOptions::default();
        let limit = ReadLimit {
            options: &options,
            memory: Some(16),
            held: 0,
        };
        let mut budget = ColdBudget::new(limit).unwrap();
        let mut bytes = Vec::<u8>::new();
        budget.reserve(&mut bytes, 8).unwrap();
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(budget.used(), 8);
        assert!(matches!(
            budget.reserve(&mut bytes, 9),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(bytes.capacity(), 8);
        assert_eq!(budget.used(), 8);
        budget.drop_vec(bytes);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn controlled_json_keeps_exact_closed_codec_values_and_bounds_scratch() {
        let options = HistoryOptions::default();
        let limit = ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        };
        let mut budget = ColdBudget::new(limit).unwrap();
        let loaded = budget
            .json::<serde_json::Value>(br#"{"a":"\uD83D\uDE80","n":18446744073709551615}"#)
            .unwrap();
        assert_eq!(loaded.value["a"], "🚀");
        assert_eq!(loaded.value["n"].as_u64(), Some(u64::MAX));
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        let large = "0,".repeat(10_000);
        assert!(matches!(
            budget.json::<serde_json::Value>(format!("[{large}0]").as_bytes()),
            Err(Error::BudgetExceeded(_))
        ));
    }

    #[test]
    fn cancellation_precedes_file_open_or_parse_allocation() {
        let options = HistoryOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            deadline: None,
        };
        let limit = ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        };
        assert!(matches!(ColdBudget::new(limit), Err(Error::Cancelled)));
    }
}
