//! Allocation and control checks for read-only cold historical decoding.
//!
//! This module never opens a writer, repairs a file, or grants managed authority.
use crate::{
    error::{Budget, BudgetKind, Error, Result},
    history::ReadLimit,
};
use serde::de::DeserializeOwned;
use std::{fs::File, io::Read, path::Path};

mod linked;
mod value;
pub(super) use linked::{head_vocabulary_controlled, read_link_controlled};

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

    pub(crate) fn available(&self) -> u64 {
        self.limit.remaining().saturating_sub(self.used)
    }

    pub(super) fn owned_limit(&self) -> Result<ReadLimit<'a>> {
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

    pub(crate) fn read_from<R: Read>(&mut self, file: &mut R, length: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Err(error) = self.append_read(file, &mut bytes, length) {
            self.drop_vec(bytes);
            return Err(error);
        }
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

    pub(crate) fn buffered(&mut self, path: &Path) -> Result<Loaded<std::io::BufReader<File>>> {
        self.check()?;
        let file = File::open(path)?;
        let charge = (self.available() / 8).clamp(64, CHUNK as u64);
        self.charge(charge)?;
        Ok(Loaded {
            value: std::io::BufReader::with_capacity(charge as usize, file),
            charge,
        })
    }

    pub(crate) fn sort_u64(&self, values: &mut [u64]) -> Result<()> {
        self.sort_by(values, Ord::cmp)
    }

    pub(crate) fn sort_by<T>(
        &self,
        values: &mut [T],
        compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    ) -> Result<()> {
        let sift = |values: &mut [T], mut root: usize, end: usize| -> Result<()> {
            loop {
                self.check()?;
                let left = root.saturating_mul(2).saturating_add(1);
                if left >= end {
                    return Ok(());
                }
                let child = if left + 1 < end && compare(&values[left], &values[left + 1]).is_lt() {
                    left + 1
                } else {
                    left
                };
                if !compare(&values[root], &values[child]).is_lt() {
                    return Ok(());
                }
                values.swap(root, child);
                root = child;
            }
        };
        let len = values.len();
        for root in (0..len / 2).rev() {
            sift(values, root, len)?;
        }
        for end in (1..len).rev() {
            self.check()?;
            values.swap(0, end);
            sift(values, 0, end)?;
        }
        self.check()
    }

    pub(crate) fn read_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.check()?;
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        self.key_step(
            "immutable file length",
            usize::try_from(length).unwrap_or(usize::MAX),
        )?;
        self.read_from(&mut file, length)
    }

    /// Read one captured descriptor's appendable prefix. A writer may trim unused
    /// allocation after its logical end; EOF is allowed here, while the caller's
    /// existing wire parser and required-term bound still validate actual data.
    /// Immutable file reads remain exact-length operations.
    pub(crate) fn read_mutable_prefix(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.check()?;
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        self.key_step(
            "mutable prefix length",
            usize::try_from(length).unwrap_or(usize::MAX),
        )?;
        let length = usize::try_from(length).map_err(|_| self.exhausted())?;
        let mut bytes = Vec::new();
        let result: Result<()> = (|| {
            while bytes.len() < length {
                self.check()?;
                let start = bytes.len();
                let end = start.saturating_add(CHUNK).min(length);
                self.reserve(&mut bytes, end)?;
                bytes.resize(end, 0);
                let mut read = start;
                while read < end {
                    self.key_step("mutable prefix read", read)?;
                    match file.read(&mut bytes[read..end]) {
                        Ok(0) => {
                            bytes.truncate(read);
                            self.check()?;
                            return Ok(());
                        }
                        Ok(count) => read += count,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            self.check()
        })();
        if let Err(error) = result {
            self.drop_vec(bytes);
            return Err(error);
        }
        Ok(bytes)
    }

    /// Validate each UTF-8 chunk before borrowing the original bytes as text. A
    /// chunk ends only at a code-point boundary and is never larger than64 KiB.
    pub(crate) fn utf8<'b>(&self, bytes: &'b [u8]) -> Result<&'b str> {
        let mut start = 0;
        while start < bytes.len() {
            self.key_step("cold UTF-8 chunk", start)?;
            let mut end = start.saturating_add(CHUNK).min(bytes.len());
            if end < bytes.len() {
                while end > start && bytes[end] & 0xc0 == 0x80 {
                    end -= 1;
                }
            }
            if end == start || std::str::from_utf8(&bytes[start..end]).is_err() {
                return Err(Error::Corrupt("historical metadata is not UTF-8".into()));
            }
            start = end;
        }
        self.check()?;
        // SAFETY: contiguous chunks cover every byte, each was independently
        // validated, and every split is a UTF-8 code-point boundary. No mutation
        // occurs while the returned borrow of the same bytes remains live.
        Ok(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    pub(crate) fn trim_text<'b>(&self, bytes: &'b [u8]) -> Result<&'b str> {
        let text = self.utf8(bytes)?;
        let mut start = 0;
        for (index, ch) in text.char_indices() {
            self.key_step("cold leading whitespace", index)?;
            if !ch.is_whitespace() {
                start = index;
                break;
            }
            start = index + ch.len_utf8();
        }
        let mut end = text.len();
        for (index, ch) in text[start..].char_indices().rev() {
            self.key_step("cold trailing whitespace", index)?;
            if !ch.is_whitespace() {
                end = start + index + ch.len_utf8();
                break;
            }
            end = start + index;
        }
        self.check()?;
        Ok(&text[start..end])
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

    /// Consume an already admitted metadata Value through its closed Serde schema.
    /// Its JSON admission remains held across both old and replacement containers.
    pub(crate) fn value<T: DeserializeOwned>(&self, value: serde_json::Value) -> Result<T> {
        self.check()?;
        let result = value::from_value(self, value)?;
        self.check()?;
        Ok(result)
    }

    /// Project fields from the same closed schema declaration before typed
    /// conversion, so unknown extensions cannot enter an uncontrolled flatten pass.
    pub(crate) fn value_fields<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        fields: &[&str],
    ) -> Result<T> {
        self.check()?;
        let result = value::from_fields(self, value, fields)?;
        self.check()?;
        Ok(result)
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[cfg(unix)]
    fn metadata_outcome_after_short_read(annotation: bool) {
        use std::{
            ffi::CString,
            io::Write,
            os::{fd::AsRawFd, unix::ffi::OsStrExt, unix::fs::MetadataExt},
            time::{Duration, Instant},
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(if annotation {
            crate::annotations::FILE
        } else {
            "commits.bin"
        });
        let native = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a live, NUL-terminated path and permissions are valid.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let original = std::fs::metadata(&path).unwrap().ino();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let mut writer = std::fs::OpenOptions::new()
                .write(true)
                .open(writer_path)
                .unwrap();
            writer.write_all(b"partial!").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut available: libc::c_int = -1;
                // SAFETY: writer is a live pipe descriptor and available points to
                // writable c_int storage of the size FIONREAD requires.
                assert_eq!(
                    unsafe { libc::ioctl(writer.as_raw_fd(), libc::FIONREAD, &mut available) },
                    0
                );
                if available == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "reader did not consume the prefix"
                );
                std::thread::yield_now();
            }
            // The reader consumed eight bytes and is blocked waiting for twelve.
            // Cancel before closing the pipe, so EOF cannot legitimately return
            // a successful missing-metadata result without observing this flag.
            flag.store(true, Ordering::SeqCst);
        });
        let options = HistoryOptions {
            cancel: Some(cancel),
            deadline: Some(Instant::now() + Duration::from_secs(10)),
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        budget.charge(17).unwrap();
        let result = if annotation {
            crate::annotations::seqs_controlled(root.path(), uuid::Uuid::new_v4(), &mut budget)
                .map(|loaded| drop(loaded.value))
        } else {
            crate::commit::walk_catalog_controlled(&path, &mut budget, |_, _| Ok(())).map(|_| ())
        };
        writer.join().unwrap();
        assert!(
            matches!(result, Err(Error::Cancelled)),
            "short metadata read returned success after actual IO cancellation: {result:?}"
        );
        assert_eq!(budget.used(), 17);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), original);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_outcome_catalog_cancellation_after_short_read() {
        metadata_outcome_after_short_read(false);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_outcome_annotation_cancellation_after_short_read() {
        metadata_outcome_after_short_read(true);
    }

    #[test]
    fn controlled_utf8_and_trim_match_borrowed_standard_results_and_cancel_midscan() {
        let options = HistoryOptions::default();
        let budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })
        .unwrap();
        for padding in (64 << 10) - 4..(64 << 10) + 4 {
            let text = "a".repeat(padding) + "🦀éλ";
            let actual = budget.utf8(text.as_bytes()).unwrap();
            assert_eq!(actual, text);
            assert_eq!(actual.as_ptr(), text.as_ptr());
        }
        for bytes in [
            vec![0x80; 70_000],
            vec![0xff],
            vec![0xf0, 0x90, 0x80],
            vec![0xc0, 0x80],
        ] {
            assert!(std::str::from_utf8(&bytes).is_err());
            assert!(matches!(budget.utf8(&bytes), Err(Error::Corrupt(_))));
        }
        for text in [
            "",
            " ",
            "\r\n\t",
            "gen-0001",
            "  gen-0001\n",
            "\u{2003}gen-0001\u{3000}",
            "🦀",
            " x 🦀 x ",
        ] {
            assert_eq!(budget.trim_text(text.as_bytes()).unwrap(), text.trim());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })
        .unwrap();
        let observer = |stage: &str, at: usize| {
            if stage == "cold UTF-8 chunk" && at > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        budget.observe_keys(&observer);
        assert!(matches!(
            budget.utf8(&vec![b'x'; 256 << 10]),
            Err(Error::Cancelled)
        ));
        cancel.store(false, Ordering::Relaxed);
        let observer = |stage: &str, at: usize| {
            if stage == "cold leading whitespace" && at == 1024 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        budget.observe_keys(&observer);
        assert!(matches!(
            budget.trim_text((" ".repeat(4096) + "gen-0001").as_bytes()),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn head_dictionary_proof_preserves_linked_positions_without_opening_index_columns() {
        use crate::{
            io::{RdfFormat, Source},
            sparql::update::update,
            store::{Store, StoreOptions},
        };
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(
            root.path(),
            StoreOptions {
                build: crate::builder::BuildOptions {
                    threads: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        store
            .load(&[Source::from_bytes(
                b"<urn:base> <urn:p> 1 .".to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        update(
            &store,
            "INSERT DATA { <urn:main> <urn:p> \"delta\" }",
            &Default::default(),
        )
        .unwrap();
        store.create_branch("dev", &Default::default()).unwrap();
        let dev = store.branch("dev").unwrap();
        update(
            &dev,
            "INSERT DATA { <urn:dev> <urn:p> \"own\" }",
            &Default::default(),
        )
        .unwrap();
        store.flush_change_log().unwrap();
        dev.flush_change_log().unwrap();
        let main = store.snapshot();
        let child = dev.snapshot();
        assert!(child.generation.link.is_some());
        let base_dir = main.generation.dir.as_deref().unwrap();
        let hidden = base_dir.join("vocab.dat.hidden");
        std::fs::rename(base_dir.join("vocab.dat"), &hidden).unwrap();
        assert!(crate::vocab::Vocab::open(base_dir).is_err());
        let options = HistoryOptions::default();
        for snapshot in [&main, &child] {
            let dir = snapshot.generation.dir.as_deref().unwrap();
            let branch_root = dir.parent().unwrap();
            let expected: Vec<_> = (0..snapshot.generation.dvocab.len())
                .map(|n| snapshot.generation.dvocab.get(n).unwrap())
                .collect();
            let before = durable_bytes(dir);
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            budget.charge(17).unwrap();
            let loaded = head_vocabulary_controlled(dir, branch_root, &mut budget).unwrap();
            assert_eq!(budget.used(), loaded.charge + 17);
            assert_eq!(loaded.value.len(), expected.len() as u64);
            for (n, term) in expected.iter().enumerate() {
                assert_eq!(loaded.value.get(n as u64).as_deref(), Some(term.as_slice()));
            }
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 17);
            assert_eq!(durable_bytes(dir), before);
        }
        let dir = child.generation.dir.as_deref().unwrap();
        let path = dir.join(super::super::link::LINK_FILE);
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["segments"][0]["branchId"] = uuid::Uuid::new_v4().to_string().into();
        let wire = serde_json::to_vec(&file).unwrap();
        std::fs::write(&path, &wire).unwrap();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        budget.charge(17).unwrap();
        assert!(matches!(
            head_vocabulary_controlled(dir, dir.parent().unwrap(), &mut budget),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(budget.used(), 17);
        assert_eq!(std::fs::read(&path).unwrap(), wire);
        std::fs::rename(hidden, base_dir.join("vocab.dat")).unwrap();
    }

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
    fn strict_file_read_captures_one_descriptor_and_still_refuses_real_truncation() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        let replacement = dir.path().join("replacement.json");
        let original = vec![b'k'; 100_000];
        std::fs::write(&path, &original).unwrap();
        std::fs::write(&replacement, b"{}").unwrap();
        let replaced = Cell::new(false);
        let observer = |stage: &str, _| {
            if stage == "immutable file length" && !replaced.replace(true) {
                std::fs::rename(&replacement, &path).unwrap();
            }
        };
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        let bytes = budget.read_file(&path).unwrap();
        assert!(replaced.get());
        assert_eq!(bytes, original);
        budget.drop_vec(bytes);
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        std::fs::write(&path, &original).unwrap();
        let truncated = Cell::new(false);
        let observer = |stage: &str, _| {
            if stage == "immutable file length" && !truncated.replace(true) {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(1)
                    .unwrap();
            }
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        assert!(
            matches!(budget.read_file(&path),Err(Error::Io(e)) if e.kind()==std::io::ErrorKind::UnexpectedEof)
        );
        assert!(truncated.get());
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"k");
    }

    #[test]
    fn mutable_prefix_reads_cancel_inside_io_and_release_owned_buffers() {
        use std::{
            cell::Cell,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutable.bin");
        let wire = vec![7; 2 << 20];
        std::fs::write(&path, &wire).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let reached = Cell::new(false);
        let observer = |stage: &str, at: usize| {
            if stage == "mutable prefix read" && at >= CHUNK && at < wire.len() {
                reached.set(true);
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(8 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        assert!(matches!(
            budget.read_mutable_prefix(&path),
            Err(Error::Cancelled)
        ));
        assert!(reached.get());
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), wire);
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
