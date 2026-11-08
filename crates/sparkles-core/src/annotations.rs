//! Commit annotations: the message a writer gave a commit, and its change digest.
//!
//! The fixed 64-byte catalog record has no room for variable-length data, so annotations
//! live in a side-file, `<root>/annotations.bin`, keyed by commit `seq`. It starts with a
//! 32-byte header: the magic `SPKANNO\0`, the format (`1`, u32 LE), flags (u32 LE, bit 0:
//! the dataset computes change digests) and the dataset UUID. One record per annotated
//! commit follows:
//!
//! | bytes | field |
//! |---|---|
//! | 0..8 | `seq` u64 LE |
//! | 8 | flags: bit 0 message, bit 1 digest |
//! | +32 | SHA-256 change digest (when flagged) |
//! | +2, +n | message length (u16 LE) and UTF-8 bytes (when flagged) |
//! | +4 | CRC-32 of the bytes before it |
//!
//! An ordinary commit appends its record durably *before* the commit point (the WAL
//! write, or the `CURRENT` switch of a bulk commit), so a durable commit always has its
//! annotation. When the sync of an ordinary commit's WAL records fails, the commit may
//! still come back at the next open, so its record is not taken back. The writer refuses
//! further commits, and the next open drops the record if the commit did not survive. A
//! commit of a group (see `store/group.rs`) only queues its record. The leader of its
//! fence writes the records of the whole prefix at the file's written end, at the same
//! time as the prefix's WAL records and terms, and acknowledges the commits only after all
//! three writes are durable. An acknowledged commit therefore has its annotation too, but
//! a commit that was not acknowledged can reach the disk without it. Its commit record
//! carries the `WAL_FLAG_ANNOTATED` bit, and the next open truncates the log from the
//! first such commit whose record is missing, when no later commit shows it was durable.
//!
//! Records are written the way the log writes its own (see `store::wal::DirectLog`). The
//! file grows in steps of zero bytes, which a buffered write and `fdatasync` make durable
//! with the records that needed the growth. Records that fit in those zeros are written
//! through a second handle opened with `O_DIRECT | O_DSYNC`, which overwrites allocated
//! blocks, changes no file size and on a device with FUA persists without a cache flush.
//! A reader stops at the first record that does not decode, so the zeros read as the end
//! of the records, as a tail that a crash cut short always did. The format is unchanged.
//!
//! A record whose commit never became durable has a `seq` above the head found at the
//! next open, which truncates it away. A torn or damaged tail is truncated too, and so
//! are the preallocated zeros.
//! In-memory stores keep annotations in memory only.

use crate::commit::{CommitInfo, rfc3339_ms};
use crate::error::{Error, Result};
use crate::store::wal::{self, Direct, DirectLog};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// File name of the side-file in a database directory.
pub const FILE: &str = "annotations.bin";
const MAGIC: &[u8; 8] = b"SPKANNO\0";
const FORMAT: u32 = 1;
const HEADER: usize = 32;
pub(crate) const REIDENTIFY_HEADER_BYTES: usize = HEADER;
const FLAG_DIGESTS: u32 = 1;
const REC_MESSAGE: u8 = 1;
const REC_DIGEST: u8 = 2;

/// Longest commit message, in bytes of UTF-8.
pub const MAX_MESSAGE_BYTES: usize = 1024;

/// The most the file grows ahead of its records at once. A step is the file's own length,
/// and at least the log's smallest step, up to this.
pub(crate) const MAX_PREALLOC: u64 = 256 << 10;

/// What is recorded about one commit besides its catalog record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Annotation {
    /// the message the writer gave the commit
    pub message: Option<Arc<str>>,
    /// the change digest (SHA-256), when the dataset computes digests
    pub digest: Option<[u8; 32]>,
}

impl Annotation {
    pub fn is_empty(&self) -> bool {
        self.message.is_none() && self.digest.is_none()
    }

    /// The digest as lowercase hex.
    pub fn digest_hex(&self) -> Option<String> {
        self.digest.as_ref().map(|d| hex(d))
    }
}

/// Check a commit message: at most [`MAX_MESSAGE_BYTES`] bytes of UTF-8 without control
/// characters (U+0000–U+001F, U+007F–U+009F). Surrounding whitespace is trimmed, and a
/// message that is empty after trimming is no message (`Ok(None)`).
pub fn validate_message(m: &str) -> Result<Option<Arc<str>>> {
    let m = m.trim();
    if m.is_empty() {
        return Ok(None);
    }
    if m.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Invalid(format!(
            "commit message is {} bytes long; the most allowed is {MAX_MESSAGE_BYTES}",
            m.len()
        )));
    }
    if let Some(c) = m.chars().find(|c| c.is_control()) {
        return Err(Error::Invalid(format!(
            "commit message contains the control character U+{:04X}",
            c as u32
        )));
    }
    Ok(Some(Arc::from(m)))
}

/// The annotations of one store: every annotated commit, in memory, backed by
/// `annotations.bin` for persistent stores.
pub(crate) struct Annotations {
    path: Option<PathBuf>,
    /// the file's written end, once the file exists
    tail: Option<Arc<Tail>>,
    /// where the next record goes: the end of the written records and of those queued
    /// for a group commit fence
    len: u64,
    map: BTreeMap<u64, Annotation>,
    /// compute change digests for new commits
    digests: bool,
    /// write records that fit in preallocated space through `O_DIRECT | O_DSYNC`
    direct: bool,
    /// the zeros of files this store replaced that it last reported to the quota, which
    /// the next [`take_prealloc`](Self::take_prealloc) withdraws
    retired: u64,
    /// a pruned file could not be opened after it replaced the old one: no record is
    /// written until the store reopens
    broken: bool,
    #[cfg(test)]
    fail_reopen: bool,
}

impl Annotations {
    pub fn memory(digests: bool) -> Annotations {
        Annotations {
            path: None,
            tail: None,
            len: 0,
            map: BTreeMap::new(),
            digests,
            direct: false,
            retired: 0,
            broken: false,
            #[cfg(test)]
            fail_reopen: false,
        }
    }

    /// Open (or create) `<root>/annotations.bin`, keeping the records of commits up to
    /// `head`. `digests` turns digests on; once on, they stay on (the header flag).
    /// `direct` lets records that fit in preallocated space be written through.
    pub fn open(
        root: &Path,
        dataset_id: uuid::Uuid,
        head: u64,
        digests: bool,
        direct: bool,
    ) -> Result<Annotations> {
        let path = root.join(FILE);
        let mut buf = Vec::new();
        match File::open(&path) {
            Ok(mut f) => {
                f.read_to_end(&mut buf)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut flags = 0;
        let mut map = BTreeMap::new();
        let mut good = 0usize;
        if !buf.is_empty() {
            match decode_header(&buf) {
                Some((id, f)) if id == dataset_id => {
                    flags = f;
                    let (m, end) = decode_records(&buf, Some(head));
                    map = m;
                    good = end;
                }
                found => {
                    // another dataset's (or an unreadable) file: set it aside, start over
                    let aside = root.join(format!("{FILE}.stale-{}", crate::commit::now_ms()));
                    tracing::warn!(
                        target: "sparkles::annotations",
                        file = %path.display(),
                        dataset = ?found.map(|f| f.0),
                        "commit annotations do not belong to this dataset; set aside as {}",
                        aside.display()
                    );
                    std::fs::rename(&path, &aside)?;
                    buf.clear();
                }
            }
        }
        if digests {
            flags |= FLAG_DIGESTS;
        }
        let digests = flags & FLAG_DIGESTS != 0;
        // only a database with annotations (or digests) gets the file
        let need_file = !buf.is_empty() || digests;
        let mut tail = None;
        let mut len = 0;
        if need_file {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)?;
            if buf.is_empty() || decode_header(&buf).map(|h| h.1) != Some(flags) {
                // a new file, or the digest flag turned on: rewrite the header in place
                let h = encode_header(dataset_id, flags);
                use std::io::{Seek, SeekFrom};
                f.seek(SeekFrom::Start(0))?;
                f.write_all(&h)?;
                good = good.max(HEADER);
            }
            if (good as u64) < f.metadata()?.len() || buf.len() > good {
                f.set_len(good as u64)?;
            }
            f.sync_all()?;
            crate::store::sync_dir(root)?;
            len = good as u64;
            tail = Some(Tail::new(f, &path, direct, len));
        }
        Ok(Annotations {
            path: Some(path),
            tail,
            len,
            map,
            digests,
            direct,
            retired: 0,
            broken: false,
            #[cfg(test)]
            fail_reopen: false,
        })
    }

    pub fn digests(&self) -> bool {
        self.digests
    }

    pub fn get(&self, seq: u64) -> Option<&Annotation> {
        self.map.get(&seq)
    }

    /// Record `a` for commit `seq` (the next commit, not yet durable) and, for a
    /// persistent store, write it durably. Undo it with [`Annotations::undo`] when the
    /// commit fails before any of its WAL records is written. `prealloc` gives the most
    /// the file may grow ahead of the record, and is asked only when it must grow. `None`
    /// when nothing was written (an empty annotation, or a store without a file).
    pub fn append(
        &mut self,
        seq: u64,
        a: Annotation,
        dataset_id: uuid::Uuid,
        prealloc: impl FnOnce() -> u64,
    ) -> Result<Option<Written>> {
        if a.is_empty() {
            return Ok(None);
        }
        let Some(tail) = self.ensure_file(dataset_id)? else {
            self.map.insert(seq, a);
            return Ok(None);
        };
        let rec = encode_record(seq, &a);
        let at = self.len;
        match tail.write(at, &rec, prealloc) {
            Ok(w) => {
                self.len += rec.len() as u64;
                self.map.insert(seq, a);
                Ok(Some(w))
            }
            Err(e) => {
                // leave no record behind that may not be on the disk, and keep the
                // header that this very append may have created
                let _ = tail.cut(at, false);
                Err(e)
            }
        }
    }

    /// Record `a` for commit `seq` of a group and queue its record for the commit's
    /// fence, which writes it with [`write_queued`] before it acknowledges the commit
    /// (none when there is nothing to write: an empty annotation, or a store without a
    /// file). The record takes its place in the file now, so the records of later commits
    /// follow it. A failed fence poisons the writer and keeps the records until reopen.
    /// The file's creation, which happens with the first record, is synced here.
    pub fn append_queued(
        &mut self,
        seq: u64,
        a: Annotation,
        dataset_id: uuid::Uuid,
    ) -> Result<Option<Queued>> {
        if a.is_empty() {
            return Ok(None);
        }
        let queued = self.ensure_file(dataset_id)?.map(|tail| {
            let data = encode_record(seq, &a);
            let at = self.len;
            self.len += data.len() as u64;
            Queued { tail, at, data }
        });
        self.map.insert(seq, a);
        Ok(queued)
    }

    /// `Err` once a pruned file could not be opened, since the store can no longer
    /// write a record that a reopen would find.
    fn usable(&self) -> Result<()> {
        if self.broken {
            return Err(Error::Poisoned);
        }
        Ok(())
    }

    /// The file's tail, creating the file with its header first if there is none yet
    /// (`None` for an in-memory store).
    fn ensure_file(&mut self, dataset_id: uuid::Uuid) -> Result<Option<Arc<Tail>>> {
        self.usable()?;
        let Some(path) = &self.path else {
            return Ok(None);
        };
        if self.tail.is_none() {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(path)?;
            let flags = if self.digests { FLAG_DIGESTS } else { 0 };
            f.write_all(&encode_header(dataset_id, flags))?;
            f.sync_all()?;
            if let Some(dir) = path.parent() {
                crate::store::sync_dir(dir)?;
            }
            self.len = HEADER as u64;
            self.tail = Some(Tail::new(f, path, self.direct, self.len));
        }
        Ok(self.tail.clone())
    }

    /// The change in the zero bytes preallocated after the written records, which are on
    /// disk but are not data, since the last report: what was reported then and what is
    /// there now. The caller reports it to the quota, which adds the zeros of every store
    /// that shares it (see [`Tail::take_prealloc`]).
    pub fn take_prealloc(&mut self) -> (u64, u64) {
        let (was, now) = self.tail.as_ref().map_or((0, 0), |t| t.take_prealloc());
        (was + std::mem::take(&mut self.retired), now)
    }

    /// Cut the zeros preallocated after the records, before the store closes: a file that
    /// is no longer written ends at its last record (an open cuts them as well). The
    /// writer takes no more commits, so no fence writes after this.
    pub fn trim(&mut self) {
        // A record still queued after a failed fence is never written, and what a failed
        // write left after the written end belongs to commits that were not acknowledged,
        // which the next open cuts as well.
        if let Some(tail) = &self.tail {
            let _ = tail.trim();
        }
    }

    /// Take back the annotation of `seq`, appended for a commit that did not happen.
    /// `Err` when the file could not be cut back (the caller refuses further writes).
    pub fn undo(&mut self, seq: u64) -> Result<()> {
        let Some(a) = self.map.remove(&seq) else {
            return Ok(());
        };
        if let Some(tail) = &self.tail {
            let n = encode_record(seq, &a).len() as u64;
            self.len -= n;
            tail.cut_synced(self.len)?;
        }
        Ok(())
    }

    /// Drop the annotations of the commits before `cutoff` (the catalog no longer has
    /// them). A file is rewritten and replaces the old one atomically.
    pub fn prune_before(&mut self, cutoff: u64, dataset_id: uuid::Uuid) -> Result<()> {
        if self.map.first_key_value().is_none_or(|(s, _)| *s >= cutoff) {
            return Ok(());
        }
        self.usable()?;
        if self.tail.as_ref().is_some_and(|t| t.end() != self.len) {
            return Err(Error::Invalid(
                "commit annotations cannot be pruned while a group commit fence has records to write".into(),
            ));
        }
        self.map = self.map.split_off(&cutoff);
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Some(tail) = &self.tail else {
            return Ok(());
        };
        let flags = if self.digests { FLAG_DIGESTS } else { 0 };
        let mut buf = encode_header(dataset_id, flags).to_vec();
        for (seq, a) in &self.map {
            buf.extend_from_slice(&encode_record(*seq, a));
        }
        crate::store::write_atomic(path, &buf)?;
        // the old file's zeros are gone with it
        self.retired += tail.take_prealloc().0;
        #[cfg(test)]
        let reopened = if self.fail_reopen {
            Err(std::io::Error::other("injected reopen failure"))
        } else {
            OpenOptions::new().read(true).write(true).open(path)
        };
        #[cfg(not(test))]
        let reopened = OpenOptions::new().read(true).write(true).open(path);
        match reopened {
            Ok(f) => {
                self.len = buf.len() as u64;
                self.tail = Some(Tail::new(f, path, self.direct, self.len));
                Ok(())
            }
            Err(e) => {
                // The old handle names the file the rename replaced, and creating the
                // file again would erase the records that the new one holds, so no
                // record is written until the store reopens. The old tail goes, since
                // its zeros were withdrawn above.
                self.broken = true;
                self.tail = None;
                Err(e.into())
            }
        }
    }

    /// In-memory stores forget the annotations of commits the catalog no longer has.
    pub fn forget_before(&mut self, seq: u64) {
        if self.path.is_none() {
            self.map = self.map.split_off(&seq);
        }
    }
}

/// A record queued for a group commit fence: its bytes and where they go in the file.
pub(crate) struct Queued {
    pub tail: Arc<Tail>,
    pub at: u64,
    pub data: Vec<u8>,
}

/// What a durable write of records did to the file.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Written {
    /// the bytes the file grew by, its records and the zeros preallocated after them
    pub grew: u64,
    /// the records went through the write-through handle
    #[cfg_attr(not(test), allow(dead_code))]
    pub direct: bool,
}

/// The written end of `annotations.bin`, which every write of records goes through. An
/// ordinary commit writes its own record here, and the leader of a group commit fence
/// writes its prefix's records. Records are written in the order of their places in the
/// file, each write starts where the last one ended, and past `end` the file holds only
/// preallocated zeros, as [`DirectLog::write`] requires.
pub(crate) struct Tail {
    io: parking_lot::Mutex<TailIo>,
}

struct TailIo {
    file: File,
    /// the path the write-through handle opens (`None`: direct writes are off)
    path: Option<PathBuf>,
    /// the file's length: past `end` it holds preallocated zero bytes
    alloc: u64,
    /// the bytes of records written to the file, where the next write starts
    end: u64,
    direct: Direct,
    /// the zeros after `end` as last reported to the quota
    reported: u64,
}

impl Tail {
    fn new(file: File, path: &Path, direct: bool, len: u64) -> Arc<Tail> {
        Arc::new(Tail {
            io: parking_lot::Mutex::new(TailIo {
                file,
                path: direct.then(|| path.to_path_buf()),
                alloc: len,
                end: len,
                direct: Direct::Untried,
                reported: 0,
            }),
        })
    }

    fn end(&self) -> u64 {
        self.io.lock().end
    }

    /// The zeros after the written records as last reported, and as they are now, which
    /// becomes the reported amount. Every change of the file, a failed write's included,
    /// is reported this way, so the quota's sum over the stores that share it stays the
    /// zeros on disk.
    pub(crate) fn take_prealloc(&self) -> (u64, u64) {
        let mut io = self.io.lock();
        let now = io.alloc.saturating_sub(io.end);
        (std::mem::replace(&mut io.reported, now), now)
    }

    /// Write the records `data` at `at`, the file's written end, durably. When they fit
    /// in the preallocated zeros, they go through the write-through handle. Otherwise
    /// they are written with a buffered write, followed by up to `prealloc()` zero bytes
    /// when the file must grow, and synchronized with `fdatasync`. A failure leaves the
    /// file in an unknown state after `at`.
    pub(crate) fn write(
        &self,
        at: u64,
        data: &[u8],
        prealloc: impl FnOnce() -> u64,
    ) -> Result<Written> {
        let mut guard = self.io.lock();
        let io = &mut *guard;
        if io.end != at {
            return Err(Error::Corrupt(format!(
                "a commit annotation would be written at byte {at} of annotations.bin, which is written up to byte {}",
                io.end
            )));
        }
        let end = at + data.len() as u64;
        if DirectLog::fits(at, data.len(), io.alloc)
            && let Some(direct) = io.direct.handle(io.path.as_deref(), &io.file)
        {
            if direct.write(at, data)? {
                io.end = end;
                return Ok(Written {
                    grew: 0,
                    direct: true,
                });
            }
            // the file system refused direct I/O: buffered from now on
            io.direct = Direct::Off;
        }
        // the handle's copy of the last block no longer holds what the file holds
        if let Direct::Open(d) = &mut io.direct {
            d.invalidate();
        }
        let before = io.alloc;
        let cap = if end > io.alloc { prealloc() } else { 0 };
        wal::write_commit(&io.file, at, &mut io.alloc, data, cap)?;
        io.file.sync_data()?;
        io.end = end;
        Ok(Written {
            grew: io.alloc - before,
            direct: false,
        })
    }

    /// Cut the file back to `at` after a failed write, with its preallocation, and
    /// synchronize the cut when `sync` is set.
    fn cut(&self, at: u64, sync: bool) -> std::io::Result<()> {
        let mut io = self.io.lock();
        io.end = at;
        let cut = io.file.set_len(at);
        match &cut {
            Ok(()) => {
                io.alloc = at;
                if let Direct::Open(d) = &mut io.direct {
                    d.invalidate();
                }
            }
            // the bytes after `at` are unknown, so no direct write may assume zeros
            Err(_) => io.direct = Direct::Off,
        }
        cut?;
        if sync {
            io.file.sync_data()?;
        }
        Ok(())
    }

    /// Cut the preallocated zeros after the written records.
    fn trim(&self) -> std::io::Result<()> {
        let mut io = self.io.lock();
        if io.alloc > io.end {
            let end = io.end;
            io.file.set_len(end)?;
            io.alloc = end;
        }
        Ok(())
    }

    /// [`cut`](Self::cut) back to `at`, the start of the last record written, durably.
    fn cut_synced(&self, at: u64) -> std::io::Result<()> {
        self.cut(at, true)
    }
}

/// Write the records that a group commit fence's commits queued, in the order of the
/// prefix, at the file's written end (see [`Tail::write`]). They must follow each other
/// and the records written before them, or nothing is written.
pub(crate) fn write_queued(records: &[&Queued], prealloc: u64) -> Result<Written> {
    let first = records[0];
    if records.windows(2).any(|w| {
        w[1].at != w[0].at + w[0].data.len() as u64 || !Arc::ptr_eq(&w[0].tail, &w[1].tail)
    }) {
        return Err(Error::Corrupt(
            "the commit annotations of a group commit fence do not follow each other".into(),
        ));
    }
    let joined;
    let data: &[u8] = if records.len() == 1 {
        &first.data
    } else {
        joined = records
            .iter()
            .flat_map(|q| q.data.iter().copied())
            .collect::<Vec<u8>>();
        &joined
    };
    first.tail.write(first.at, data, || prealloc)
}

/// The bytes of the record of an annotation with `message` and, when `digest` is set, a
/// change digest (0 when it has neither, since such a commit has no record).
pub(crate) fn record_len(message: Option<&str>, digest: bool) -> u64 {
    if message.is_none() && !digest {
        return 0;
    }
    let message = message.map_or(0, |m| 2 + m.len().min(u16::MAX as usize));
    (8 + 1 + if digest { 32 } else { 0 } + message + 4) as u64
}

/// Where the complete, valid records of an annotations file's content end (past it lie
/// preallocated zeros or a torn tail), or `None` for a file without a valid header.
#[cfg(test)]
pub(crate) fn records_end(buf: &[u8]) -> Option<usize> {
    decode_header(buf)?;
    Some(decode_records(buf, None).1)
}

/// The commits that have an annotation in `<root>/annotations.bin`, for the replay of the
/// write-ahead log (see `WAL_FLAG_ANNOTATED`). `None` when the file is absent or does
/// not belong to this dataset, in which case a flagged commit is not held against it: the
/// file existed durably before the first flagged commit was written, so such a state is
/// damage from outside, not a crash.
pub(crate) fn seqs_of(
    root: &Path,
    dataset_id: uuid::Uuid,
) -> Option<std::collections::BTreeSet<u64>> {
    let buf = std::fs::read(root.join(FILE)).ok()?;
    match decode_header(&buf) {
        Some((id, _)) if id == dataset_id => {
            Some(decode_records(&buf, None).0.into_keys().collect())
        }
        _ => None,
    }
}

/// Read the annotations of a database without opening it (no lock; for `sparkles log`
/// next to a running server): every complete, valid record.
pub fn read(root: &Path) -> Result<BTreeMap<u64, Annotation>> {
    let buf = match std::fs::read(root.join(FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    if decode_header(&buf).is_none() {
        return Ok(BTreeMap::new());
    }
    Ok(decode_records(&buf, None).0)
}

/// Move the annotations of a closed database to its new dataset id (see
/// [`commit::reidentify`](crate::commit::reidentify)): commits keep their numbers, so
/// their annotations stay valid.
pub(crate) fn reidentify(root: &Path, old: uuid::Uuid, new: uuid::Uuid) -> Result<()> {
    let path = root.join(FILE);
    let mut buf = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if let Some((id, flags)) = decode_header(&buf)
        && id == old
    {
        buf[..HEADER].copy_from_slice(&encode_header(new, flags));
        crate::store::write_atomic(&path, &buf)?;
    }
    Ok(())
}

/// Pure fixed-header transform for the genuine streaming destination producer.
/// All records stay on its retained captured descriptor and are copied once.
pub(crate) fn reidentified_header(
    header: &[u8; REIDENTIFY_HEADER_BYTES],
    old: uuid::Uuid,
    new: uuid::Uuid,
) -> Result<[u8; REIDENTIFY_HEADER_BYTES]> {
    if &header[..8] == MAGIC && header[8..12] != FORMAT.to_le_bytes() {
        return Err(Error::Unsupported(
            "original annotations have an unknown format".into(),
        ));
    }
    let (owner, flags) = decode_header(header)
        .ok_or_else(|| Error::Corrupt("original annotation header is invalid".into()))?;
    if old.is_nil() || new.is_nil() || owner != old {
        return Err(Error::Corrupt(
            "original annotations belong to another source".into(),
        ));
    }
    Ok(encode_header(new, flags))
}

fn encode_header(id: uuid::Uuid, flags: u32) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[12..16].copy_from_slice(&flags.to_le_bytes());
    h[16..32].copy_from_slice(id.as_bytes());
    h
}

fn decode_header(buf: &[u8]) -> Option<(uuid::Uuid, u32)> {
    if buf.len() < HEADER || &buf[..8] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(buf[8..12].try_into().ok()?) != FORMAT {
        return None;
    }
    let flags = u32::from_le_bytes(buf[12..16].try_into().ok()?);
    let id = uuid::Uuid::from_slice(&buf[16..32]).ok()?;
    Some((id, flags))
}

fn encode_record(seq: u64, a: &Annotation) -> Vec<u8> {
    let mut r = Vec::with_capacity(64);
    r.extend_from_slice(&seq.to_le_bytes());
    let mut flags = 0u8;
    if a.message.is_some() {
        flags |= REC_MESSAGE;
    }
    if a.digest.is_some() {
        flags |= REC_DIGEST;
    }
    r.push(flags);
    if let Some(d) = &a.digest {
        r.extend_from_slice(d);
    }
    if let Some(m) = &a.message {
        let b = m.as_bytes();
        let n = b.len().min(u16::MAX as usize);
        r.extend_from_slice(&(n as u16).to_le_bytes());
        r.extend_from_slice(&b[..n]);
    }
    let mut c = flate2::Crc::new();
    c.update(&r);
    r.extend_from_slice(&c.sum().to_le_bytes());
    r
}

/// The records after the header, up to the first incomplete or damaged one (or, with
/// `head`, the first one for a later commit), and the offset where the good ones end.
fn decode_records(buf: &[u8], head: Option<u64>) -> (BTreeMap<u64, Annotation>, usize) {
    let mut map = BTreeMap::new();
    let mut pos = HEADER;
    while let Some((seq, a, next)) = decode_record(buf, pos) {
        if head.is_some_and(|h| seq > h) {
            break;
        }
        map.insert(seq, a);
        pos = next;
    }
    (map, pos)
}

/// Borrow the wire fields so read-only probes need not allocate discarded messages.
type BorrowedRecord<'a> = (u64, Option<&'a str>, Option<[u8; 32]>, usize);

fn decode_record_borrowed(buf: &[u8], start: usize) -> Option<BorrowedRecord<'_>> {
    let mut pos = start;
    let take = |pos: &mut usize, n: usize| -> Option<&[u8]> {
        let s = buf.get(*pos..pos.checked_add(n)?)?;
        *pos += n;
        Some(s)
    };
    let seq = u64::from_le_bytes(take(&mut pos, 8)?.try_into().ok()?);
    let flags = take(&mut pos, 1)?[0];
    if flags & !(REC_MESSAGE | REC_DIGEST) != 0 {
        return None;
    }
    let digest = if flags & REC_DIGEST != 0 {
        Some(take(&mut pos, 32)?.try_into().ok()?)
    } else {
        None
    };
    let message = if flags & REC_MESSAGE != 0 {
        let n = u16::from_le_bytes(take(&mut pos, 2)?.try_into().ok()?) as usize;
        Some(std::str::from_utf8(take(&mut pos, n)?).ok()?)
    } else {
        None
    };
    let body_end = pos;
    let crc = u32::from_le_bytes(take(&mut pos, 4)?.try_into().ok()?);
    let mut c = flate2::Crc::new();
    c.update(&buf[start..body_end]);
    (c.sum() == crc).then_some((seq, message, digest, pos))
}

fn decode_record(buf: &[u8], start: usize) -> Option<(u64, Annotation, usize)> {
    let (seq, message, digest, end) = decode_record_borrowed(buf, start)?;
    Some((
        seq,
        Annotation {
            message: message.map(Arc::from),
            digest,
        },
        end,
    ))
}

/// Read only sequence membership, preserving the existing annotation valid-prefix
/// semantics. The returned membership is recovery metadata, never actor authority.
/// This does not create, repair, rename or truncate the annotation file.
pub(crate) fn seqs_controlled(
    root: &Path,
    dataset_id: uuid::Uuid,
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<crate::store::cold::Loaded<Option<Vec<u64>>>> {
    use crate::store::cold::Loaded;
    let loaded = match budget.buffered(&root.join(FILE)) {
        Ok(loaded) => loaded,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                value: None,
                charge: 0,
            });
        }
        Err(error) => return Err(error),
    };
    let mut reader = loaded.value;
    let result = seqs_from(&mut reader, dataset_id, budget);
    drop(reader);
    budget.release(loaded.charge);
    result
}

/// Scan an already retained byte source with the same valid-prefix semantics.
/// This helper neither opens a path nor supplies dataset/annotation authority.
pub(crate) fn seqs_from<R: Read>(
    reader: &mut R,
    dataset_id: uuid::Uuid,
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<crate::store::cold::Loaded<Option<Vec<u64>>>> {
    use crate::store::cold::Loaded;
    let before = budget.used();
    let result = (|| {
        let mut header = [0; HEADER];
        let result: Result<Option<Vec<u64>>> = (|| {
            match reader.read_exact(&mut header[..12]) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            budget.check()?;
            if &header[..8] == MAGIC {
                crate::formats::ANNOTATIONS.check(
                    "retained commit annotations",
                    u32::from_le_bytes(header[8..12].try_into().unwrap()),
                    None,
                )?;
            }
            match reader.read_exact(&mut header[12..]) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            budget.check()?;
            if decode_header(&header).map(|h| h.0) != Some(dataset_id) {
                return Ok(None);
            }
            let mut seqs = Vec::new();
            let mut wire = Vec::new();
            let scan = (|| {
                loop {
                    budget.key_step("annotation record", seqs.len())?;
                    wire.clear();
                    if let Err(error) = budget.append_read(reader, &mut wire, 9) {
                        if matches!(&error, Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
                        {
                            break;
                        }
                        return Err(error);
                    }
                    let flags = wire[8];
                    if flags & !(REC_MESSAGE | REC_DIGEST) != 0 {
                        break;
                    }
                    let extra = if flags & REC_DIGEST != 0 { 32 } else { 0 };
                    let extra = extra + if flags & REC_MESSAGE != 0 { 2 } else { 0 };
                    if let Err(error) = budget.append_read(reader, &mut wire, extra) {
                        if matches!(&error, Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
                        {
                            break;
                        }
                        return Err(error);
                    }
                    let message = if flags & REC_MESSAGE != 0 {
                        u16::from_le_bytes(wire[wire.len() - 2..].try_into().unwrap()) as u64
                    } else {
                        0
                    };
                    if let Err(error) = budget.append_read(reader, &mut wire, message + 4) {
                        if matches!(&error, Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
                        {
                            break;
                        }
                        return Err(error);
                    }
                    let Some((seq, _, _, _)) = decode_record_borrowed(&wire, 0) else {
                        break;
                    };
                    let need = seqs.len().saturating_add(1);
                    budget.reserve(&mut seqs, need)?;
                    seqs.push(seq);
                }
                budget.sort_u64(&mut seqs)?;
                let mut keep = 0;
                for index in 0..seqs.len() {
                    if index.is_multiple_of(1024) {
                        budget.check()?;
                    }
                    if keep == 0 || seqs[index] != seqs[keep - 1] {
                        seqs[keep] = seqs[index];
                        keep += 1;
                    }
                }
                seqs.truncate(keep);
                Ok(())
            })();
            budget.drop_vec(wire);
            scan?;
            Ok(Some(seqs))
        })();
        let value = result?;
        Ok(Loaded {
            value,
            charge: budget.used() - before,
        })
    })();
    let result = result.and_then(|value| {
        budget.check()?;
        Ok(value)
    });
    if result.is_err() {
        budget.release(budget.used() - before);
    }
    result
}

pub(crate) fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// The change digest of a commit (`sparkles-commit-digest-v1`): SHA-256 over
///
/// ```text
/// sparkles-commit-digest-v1\n
/// <dataset id>\n<seq>\n<hex of the parent's digest>\n<timestamp>\n<kind>\n
/// -<canonical N-Quads line of a deleted quad>\n …   (sorted by UTF-8 bytes)
/// +<canonical N-Quads line of an inserted quad>\n … (sorted by UTF-8 bytes)
/// ```
///
/// `parent` is the parent's digest, or 32 zero bytes when it has none. The quad lines
/// end in ` .` like N-Quads lines; blank nodes carry the store's internal labels.
pub fn change_digest(
    dataset_id: uuid::Uuid,
    c: &CommitInfo,
    parent: Option<&[u8; 32]>,
    mut deleted: Vec<String>,
    mut inserted: Vec<String>,
) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    let zero = [0u8; 32];
    h.update(b"sparkles-commit-digest-v1\n");
    h.update(
        format!(
            "{dataset_id}\n{}\n{}\n{}\n{}\n",
            c.seq,
            hex(parent.unwrap_or(&zero)),
            rfc3339_ms(c.timestamp_ms),
            c.kind.name()
        )
        .as_bytes(),
    );
    deleted.sort_unstable();
    inserted.sort_unstable();
    for (sign, lines) in [(b"-", &deleted), (b"+", &inserted)] {
        for l in lines {
            h.update(sign);
            h.update(l.as_bytes());
            h.update(b"\n");
        }
    }
    h.finalize().into()
}

/// A quad as a canonical N-Quads line without the newline (RDF 1.2 N-Quads §3):
/// `<s> <p> <o> <g> .`, or `<s> <p> <o> .` in the default graph.
pub fn nquads_line(q: &oxrdf::Quad) -> String {
    match &q.graph_name {
        oxrdf::GraphName::DefaultGraph => {
            format!("{} {} {} .", q.subject, q.predicate, q.object)
        }
        g => format!("{} {} {} {g} .", q.subject, q.predicate, q.object),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each change of the zeros after the records is reported as what was reported last
    /// and what is there now, the cut of an undone record included, so that a quota that
    /// several stores share keeps their sum.
    #[test]
    fn the_preallocated_zeros_are_reported_as_they_change() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut a = Annotations::open(root.path(), id, 0, false, true).unwrap();
        assert_eq!(a.take_prealloc(), (0, 0), "no file yet");
        let message = |m: &str| Annotation {
            message: Some(Arc::from(m)),
            digest: None,
        };
        let w = a
            .append(1, message("one"), id, || MAX_PREALLOC)
            .unwrap()
            .unwrap();
        let (was, zeros) = a.take_prealloc();
        assert_eq!(was, 0);
        assert!(
            zeros >= 64 << 10,
            "the first record grew the file by a step"
        );
        let path = root.path().join(FILE);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(w.grew, len - HEADER as u64);
        assert_eq!(
            records_end(&std::fs::read(&path).unwrap()),
            Some((len - zeros) as usize)
        );
        a.append(2, message("two"), id, || MAX_PREALLOC)
            .unwrap()
            .unwrap();
        let (was, now) = a.take_prealloc();
        assert_eq!(was, zeros);
        assert!(now < zeros, "the second record took some of the zeros");
        // an undone record is cut with the zeros after it
        a.undo(2).unwrap();
        assert_eq!(a.take_prealloc(), (now, 0));
        assert_eq!(a.take_prealloc(), (0, 0));
        let end = records_end(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), end as u64);
        // a pruned file is replaced, and its zeros are withdrawn
        a.append(2, message("two"), id, || MAX_PREALLOC)
            .unwrap()
            .unwrap();
        let (_, before) = a.take_prealloc();
        assert!(before > 0);
        a.prune_before(2, id).unwrap();
        assert_eq!(a.take_prealloc(), (before, 0));
        drop(a);
        let reopened = Annotations::open(root.path(), id, 2, false, true).unwrap();
        assert!(reopened.get(1).is_none());
        assert_eq!(reopened.get(2).unwrap().message.as_deref(), Some("two"));
    }

    /// A pruned file that cannot be opened after it replaced the old one leaves the
    /// store unable to write records, rather than creating the file again over the
    /// records it keeps.
    #[test]
    fn a_pruned_file_that_cannot_be_reopened_refuses_records() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut a = Annotations::open(root.path(), id, 0, false, true).unwrap();
        let message = |m: &str| Annotation {
            message: Some(Arc::from(m)),
            digest: None,
        };
        for (seq, m) in [(1, "one"), (2, "two")] {
            a.append(seq, message(m), id, || MAX_PREALLOC).unwrap();
        }
        let (_, zeros) = a.take_prealloc();
        assert!(zeros > 0);
        a.fail_reopen = true;
        assert!(a.prune_before(2, id).is_err());
        // the replaced file's zeros are withdrawn once, and a close withdraws nothing more
        assert_eq!(a.take_prealloc(), (zeros, 0));
        a.trim();
        assert_eq!(a.take_prealloc(), (0, 0));
        assert!(a.append(3, message("three"), id, || MAX_PREALLOC).is_err());
        assert!(a.append_queued(3, message("three"), id).is_err());
        assert!(a.prune_before(3, id).is_err());
        drop(a);
        let reopened = Annotations::open(root.path(), id, 3, false, true).unwrap();
        assert_eq!(reopened.get(2).unwrap().message.as_deref(), Some("two"));
        assert!(reopened.get(3).is_none());
    }

    /// A close cuts the zeros even when a failed fence left a record queued that will
    /// never be written, and reports that they are gone.
    #[test]
    fn a_close_after_a_failed_fence_cuts_the_zeros() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut a = Annotations::open(root.path(), id, 0, false, true).unwrap();
        let message = |m: &str| Annotation {
            message: Some(Arc::from(m)),
            digest: None,
        };
        a.append(1, message("one"), id, || MAX_PREALLOC).unwrap();
        let (_, zeros) = a.take_prealloc();
        assert!(zeros > 0);
        let _never_written = a.append_queued(2, message("two"), id).unwrap().unwrap();
        a.trim();
        assert_eq!(a.take_prealloc(), (zeros, 0));
        let path = root.path().join(FILE);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(records_end(&bytes), Some(bytes.len()));
    }

    #[test]
    fn controlled_membership_refuses_future_primary_version_without_repair() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FILE);
        let id = uuid::Uuid::new_v4();
        let options = HistoryOptions::default();
        for version in [1u32, 99] {
            for len in [0, 7, 8, 11, 12, 13, HEADER - 1, HEADER] {
                for known_magic in [true, false] {
                    let mut bytes = encode_header(id, 0);
                    bytes[8..12].copy_from_slice(&version.to_le_bytes());
                    if !known_magic {
                        bytes[0] ^= 1;
                    }
                    let bytes = &bytes[..len];
                    std::fs::write(&path, bytes).unwrap();
                    let mut budget = ColdBudget::new(ReadLimit {
                        options: &options,
                        memory: Some(4096),
                        held: 0,
                    })
                    .unwrap();
                    let result = seqs_controlled(root.path(), id, &mut budget);
                    if known_magic && version == 99 && len >= 12 {
                        assert!(
                            matches!(result, Err(Error::Unsupported(_))),
                            "recognizable future annotation prefix must refuse: len={len}"
                        );
                    } else {
                        let loaded = result.unwrap();
                        assert_eq!(
                            loaded.value,
                            (known_magic && version == 1 && len == HEADER).then(Vec::new)
                        );
                        assert_eq!(loaded.charge, 0);
                    }
                    assert_eq!(budget.used(), 0);
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                }
            }
        }
    }

    #[test]
    fn controlled_annotation_membership_preserves_valid_prefix_and_bytes() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut bytes = encode_header(id, 0).to_vec();
        for seq in [9, 2, 9, 5] {
            bytes.extend(encode_record(
                seq,
                &Annotation {
                    message: Some(Arc::from("z".repeat(60_000))),
                    digest: Some([3; 32]),
                },
            ));
        }
        bytes.extend_from_slice(&[0; 12]); // incomplete/bad trailing record
        std::fs::write(root.path().join(FILE), &bytes).unwrap();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(512 << 10),
            held: 0,
        })
        .unwrap();
        let loaded = seqs_controlled(root.path(), id, &mut budget).unwrap();
        assert_eq!(loaded.value.as_deref(), Some([2, 5, 9].as_slice()));
        assert_eq!(budget.used(), loaded.charge);
        assert_eq!(
            loaded.charge,
            loaded.value.as_ref().unwrap().capacity() as u64 * 8
        );
        assert_eq!(std::fs::read(root.path().join(FILE)).unwrap(), bytes);
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        let mismatch = seqs_controlled(root.path(), uuid::Uuid::new_v4(), &mut budget).unwrap();
        assert!(mismatch.value.is_none());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn retained_annotation_reader_preserves_valid_prefix_and_version_refusal() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        struct Chunks<'a>(&'a [u8]);
        impl Read for Chunks<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = output.len().min(self.0.len()).min(3);
                output[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        let id = uuid::Uuid::new_v4();
        let mut bytes = encode_header(id, 0).to_vec();
        for seq in [7, 1, 7] {
            bytes.extend(encode_record(
                seq,
                &Annotation {
                    message: Some(Arc::from("retained")),
                    ..Default::default()
                },
            ));
        }
        let torn = bytes.len();
        bytes.extend(encode_record(9, &Annotation::default()));
        *bytes.last_mut().unwrap() ^= 1;
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(8192),
            held: 0,
        })
        .unwrap();
        let loaded = seqs_from(&mut Chunks(&bytes), id, &mut budget).unwrap();
        assert_eq!(loaded.value.as_deref(), Some([1, 7].as_slice()));
        assert_eq!(budget.used(), loaded.charge);
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        assert!(torn < bytes.len());
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let result = seqs_from(&mut Chunks(&bytes), id, &mut budget);
        assert!(matches!(result, Err(Error::Unsupported(_))));
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn controlled_annotation_scan_refuses_and_cancels_without_repair() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut bytes = encode_header(id, 0).to_vec();
        for seq in 0..100 {
            bytes.extend(encode_record(seq, &Annotation::default()));
        }
        std::fs::write(root.path().join(FILE), &bytes).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(64),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            seqs_controlled(root.path(), id, &mut budget),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(budget.used(), 0);
        let observer = |stage: &str, at: usize| {
            if stage == "annotation record" && at == 8 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        assert!(matches!(
            seqs_controlled(root.path(), id, &mut budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(root.path().join(FILE)).unwrap(), bytes);
    }

    #[test]
    fn messages_are_trimmed_and_checked() {
        assert_eq!(
            validate_message("  fix labels ").unwrap().as_deref(),
            Some("fix labels")
        );
        assert_eq!(validate_message("   ").unwrap(), None);
        assert_eq!(
            validate_message("héllo ✓").unwrap().as_deref(),
            Some("héllo ✓")
        );
        assert!(validate_message("a\nb").is_err());
        assert!(validate_message("a\u{7f}b").is_err());
        assert!(validate_message("a\u{85}b").is_err());
        assert!(validate_message(&"x".repeat(MAX_MESSAGE_BYTES)).is_ok());
        assert!(validate_message(&"x".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    }

    #[test]
    fn records_round_trip_and_stop_at_damage() {
        let a = Annotation {
            message: Some(Arc::from("m")),
            digest: Some([7; 32]),
        };
        let b = Annotation {
            message: Some(Arc::from("second")),
            digest: None,
        };
        let id = uuid::Uuid::new_v4();
        let mut buf = encode_header(id, 0).to_vec();
        buf.extend(encode_record(3, &a));
        let end_a = buf.len();
        buf.extend(encode_record(5, &b));
        let (m, end) = decode_records(&buf, None);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&3], a);
        assert_eq!(m[&5], b);
        assert_eq!(end, buf.len());
        // a commit past the head stops the scan
        let (m, end) = decode_records(&buf, Some(4));
        assert_eq!((m.len(), end), (1, end_a));
        // a flipped byte or a torn tail ends the valid records
        let mut bad = buf.clone();
        bad[end_a + 10] ^= 1;
        assert_eq!(decode_records(&bad, None).1, end_a);
        assert_eq!(decode_records(&buf[..buf.len() - 1], None).1, end_a);
    }

    #[test]
    fn digest_sorts_lines_and_chains() {
        let c = CommitInfo {
            seq: 1,
            timestamp_ms: 1000,
            kind: crate::commit::CommitKind::Update,
            inserted: 2,
            deleted: 0,
            quads: 2,
            generation: 0,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        };
        let id = uuid::Uuid::nil();
        let l = |s: &str| s.to_string();
        let d1 = change_digest(
            id,
            &c,
            None,
            vec![],
            vec![l("<a> <b> <c> ."), l("<x> <y> <z> .")],
        );
        let d2 = change_digest(
            id,
            &c,
            None,
            vec![],
            vec![l("<x> <y> <z> ."), l("<a> <b> <c> .")],
        );
        assert_eq!(d1, d2);
        // the exact preimage
        use sha2::Digest;
        let text = format!(
            "sparkles-commit-digest-v1\n{id}\n1\n{}\n1970-01-01T00:00:01.000Z\nupdate\n+<a> <b> <c> .\n+<x> <y> <z> .\n",
            "0".repeat(64)
        );
        let want: [u8; 32] = sha2::Sha256::digest(text.as_bytes()).into();
        assert_eq!(d1, want);
        let d3 = change_digest(id, &c, Some(&d1), vec![], vec![l("<a> <b> <c> .")]);
        let d4 = change_digest(id, &c, None, vec![], vec![l("<a> <b> <c> .")]);
        assert_ne!(d3, d4);
    }
    #[test]
    fn imported_annotation_header_preserves_flags_and_rejects_foreign_or_malformed_source() {
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        let header = encode_header(old, FLAG_DIGESTS | 8);
        let output = reidentified_header(&header, old, new).unwrap();
        assert_eq!(decode_header(&output), Some((new, FLAG_DIGESTS | 8)));
        assert_eq!(&output[..16], &header[..16]);
        assert_eq!(decode_header(&header), Some((old, FLAG_DIGESTS | 8)));
        assert!(reidentified_header(&header, uuid::Uuid::new_v4(), new).is_err());
        assert!(reidentified_header(&header, old, uuid::Uuid::nil()).is_err());
        let mut unknown = header;
        unknown[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(reidentified_header(&unknown, old, new).is_err());
    }
}
