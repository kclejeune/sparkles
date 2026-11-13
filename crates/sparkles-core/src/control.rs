//! The control keyspace (spec C26 §2 and §7): versioned key/value records of a store's
//! owners, such as the job ledger, that live outside the RDF dataset.
//!
//! The keyspace is a [`ControlState`], an immutable ordered map held in memory and
//! replaced whole on every change, so a reader holds one consistent state. It changes in
//! [`Frame`]s, each numbered by the next control sequence number. A frame that belongs to
//! a commit with a net change to the quads rides in that commit's write-ahead log
//! transaction as `WAL_CONTROL` records (see [`chunks`] and [`Frame::from_chunks`]). A
//! frame without quads is appended to [`LOG_FILE`] at the dataset root ([`log_record`],
//! [`read_log`]). [`CHECKPOINT_FILE`] holds the whole state after one sequence number
//! ([`encode_checkpoint`], [`decode_checkpoint`]), and [`recover`] merges the checkpoint
//! and the frames of both logs at open (spec C26 §7.2).
//!
//! # Formats
//!
//! A frame, version [`FRAME_VERSION`]: a version byte, the frame's length (`u32`), its
//! control sequence number (`u64`), the business commit it follows (`u64`), a timestamp
//! in milliseconds (`i64`) and the number of its operations (`u32`), then each operation:
//! `1` (put) or `2` (delete), the key's length (`u16`) and the key, and for a put the
//! value's length (`u32`) and the value. Every integer is little-endian.
//!
//! `control.log`, version [`LOG_VERSION`]: a 16-byte header of [`LOG_MAGIC`], the
//! version (`u32`) and four zero bytes, then one record per frame: the frame's length
//! (`u32`), the CRC-32 of the frame (`u32`) and the frame.
//!
//! `control.bin`, version [`CHECKPOINT_VERSION`]: [`CHECKPOINT_MAGIC`], the version
//! (`u32`), four zero bytes, the sequence number (`u64`), the commit it follows (`u64`),
//! the number of entries (`u64`), each entry's key length (`u16`), key, value length
//! (`u32`) and value in key order, and a SHA-256 of every byte before it.

use crate::error::{Error, Result};
use sha2::Digest;
use std::path::Path;
use std::sync::Arc;

/// The control log at the dataset root.
pub const LOG_FILE: &str = "control.log";

/// The control checkpoint at the dataset root.
pub const CHECKPOINT_FILE: &str = "control.bin";

/// The first eight bytes of `control.log`.
pub const LOG_MAGIC: [u8; 8] = *b"\x89SPKCTL\n";

/// The first eight bytes of `control.bin`.
pub const CHECKPOINT_MAGIC: [u8; 8] = *b"\x89SPKCKP\n";

/// The `control.log` version this build reads and writes.
pub const LOG_VERSION: u32 = 1;

/// The `control.bin` version this build reads and writes.
pub const CHECKPOINT_VERSION: u32 = 1;

/// The frame version this build reads and writes.
pub const FRAME_VERSION: u8 = 1;

/// The bytes of the `control.log` header.
pub const LOG_HEADER_BYTES: u64 = 16;

/// The longest key, in bytes.
pub const MAX_KEY: usize = 512;

/// The longest value, in bytes.
pub const MAX_VALUE: usize = 4 << 20;

/// The longest frame, in bytes.
pub const MAX_FRAME: usize = 16 << 20;

/// The bytes of a frame's fixed header.
const FRAME_HEADER: usize = 33;

/// The payload bytes of one `WAL_CONTROL` record.
pub const CHUNK: usize = 32;

/// The key of the origin entry a backup capture writes into the checkpoint it renders
/// (spec C26 §7.3). Its value is the JSON of an [`Origin`].
pub const ORIGIN_KEY: &[u8] = b"core/origin";

/// The bytes past which `control.log` is replaced by a checkpoint, when it has also
/// outgrown the last checkpoint.
pub const LOG_CHECKPOINT_BYTES: u64 = 64 << 20;

/// Where a checkpoint with an origin entry came from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Origin {
    /// `capture`, the only origin this build writes
    pub kind: String,
    /// the source dataset
    pub dataset: uuid::Uuid,
    /// the captured commit
    pub commit: u64,
    /// the capture's label, such as the backup's name
    pub label: String,
    /// when the capture was taken, in milliseconds since the epoch
    pub time_ms: i64,
}

/// One change of a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Put(Box<[u8]>, Arc<[u8]>),
    Delete(Box<[u8]>),
}

impl Op {
    pub fn key(&self) -> &[u8] {
        match self {
            Op::Put(k, _) | Op::Delete(k) => k,
        }
    }
}

/// The changes of one transaction to the control keyspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// the control sequence number
    pub seq: u64,
    /// the business commit it follows: the commit it rides in, for a frame of the
    /// write-ahead log, and the head when it was written, for a frame of `control.log`
    pub follows: u64,
    pub time_ms: i64,
    pub ops: Vec<Op>,
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("control keyspace: {what}"))
}

/// Check a key and, for a put, its value against the limits.
pub fn check_entry(key: &[u8], value: Option<&[u8]>) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY {
        return Err(Error::invalid(format!(
            "a control key is 1 to {MAX_KEY} bytes, not {}",
            key.len()
        )));
    }
    if let Some(v) = value
        && v.len() > MAX_VALUE
    {
        return Err(Error::invalid(format!(
            "a control value is at most {MAX_VALUE} bytes, not {}",
            v.len()
        )));
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or_else(|| corrupt("a record ends early"))?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

impl Frame {
    /// The frame's bytes. Fails when the frame exceeds [`MAX_FRAME`] or an entry its
    /// limits.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(FRAME_HEADER + self.ops.len() * 64);
        out.push(FRAME_VERSION);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.follows.to_le_bytes());
        out.extend_from_slice(&self.time_ms.to_le_bytes());
        out.extend_from_slice(&(self.ops.len() as u32).to_le_bytes());
        for op in &self.ops {
            match op {
                Op::Put(k, v) => {
                    check_entry(k, Some(v))?;
                    out.push(1);
                    out.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    out.extend_from_slice(k);
                    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    out.extend_from_slice(v);
                }
                Op::Delete(k) => {
                    check_entry(k, None)?;
                    out.push(2);
                    out.extend_from_slice(&(k.len() as u16).to_le_bytes());
                    out.extend_from_slice(k);
                }
            }
            if out.len() > MAX_FRAME {
                return Err(Error::invalid(format!(
                    "a control frame is at most {MAX_FRAME} bytes"
                )));
            }
        }
        let len = out.len() as u32;
        out[1..5].copy_from_slice(&len.to_le_bytes());
        Ok(out)
    }

    /// The frame `bytes` hold exactly. A version this build does not read is refused
    /// with [`Error::Unsupported`].
    pub fn decode(bytes: &[u8]) -> Result<Frame> {
        let mut r = Reader { bytes, at: 0 };
        let version = r.u8()?;
        if version != FRAME_VERSION {
            return Err(Error::Unsupported(format!(
                "control frame version {version}; Sparkles {} reads version {FRAME_VERSION}",
                crate::formats::SPARKLES_VERSION
            )));
        }
        let len = r.u32()? as usize;
        if len != bytes.len() || len > MAX_FRAME {
            return Err(corrupt(format!(
                "a frame declares {len} bytes in a record of {}",
                bytes.len()
            )));
        }
        let seq = r.u64()?;
        let follows = r.u64()?;
        let time_ms = r.u64()? as i64;
        let count = r.u32()? as usize;
        // every operation takes at least four bytes
        if count > len / 4 {
            return Err(corrupt("a frame counts more operations than it holds"));
        }
        let mut ops = Vec::with_capacity(count);
        for _ in 0..count {
            let op = r.u8()?;
            let klen = r.u16()? as usize;
            let key: Box<[u8]> = r.take(klen)?.into();
            match op {
                1 => {
                    let vlen = r.u32()? as usize;
                    let value: Arc<[u8]> = r.take(vlen)?.into();
                    check_entry(&key, Some(&value)).map_err(|e| corrupt(e))?;
                    ops.push(Op::Put(key, value));
                }
                2 => {
                    check_entry(&key, None).map_err(|e| corrupt(e))?;
                    ops.push(Op::Delete(key));
                }
                other => return Err(corrupt(format!("unknown operation {other}"))),
            }
        }
        if r.at != bytes.len() {
            return Err(corrupt("a frame has bytes after its last operation"));
        }
        Ok(Frame {
            seq,
            follows,
            time_ms,
            ops,
        })
    }

    /// The frame carried by the payloads of a transaction's `WAL_CONTROL` records, in
    /// order: the frame's declared length, then only zeros to the end of the last
    /// record (spec C26 §7).
    pub fn from_chunks(payload: &[u8]) -> Result<Frame> {
        if payload.len() < FRAME_HEADER {
            return Err(corrupt("control records shorter than a frame header"));
        }
        let len = u32::from_le_bytes(payload[1..5].try_into().unwrap()) as usize;
        if len < FRAME_HEADER || len > payload.len() || payload.len() - len >= CHUNK {
            return Err(corrupt(format!(
                "a frame of {len} bytes in {} bytes of control records",
                payload.len()
            )));
        }
        if payload[len..].iter().any(|b| *b != 0) {
            return Err(corrupt(
                "control records pad their frame with non-zero bytes",
            ));
        }
        Frame::decode(&payload[..len])
    }
}

/// `frame` cut into the 32-byte payloads of `WAL_CONTROL` records, the last one padded
/// with zeros.
pub fn chunks(frame: &[u8]) -> impl Iterator<Item = [u8; CHUNK]> + '_ {
    frame.chunks(CHUNK).map(|c| {
        let mut out = [0; CHUNK];
        out[..c.len()].copy_from_slice(c);
        out
    })
}

/// The control keyspace after one sequence number: an immutable ordered map that a
/// transaction copies only along the paths it changes.
#[derive(Clone, Debug, Default)]
pub struct ControlState {
    /// the last frame applied (0 for the empty state of a new keyspace)
    pub seq: u64,
    /// the business commit the last frame follows
    pub follows: u64,
    entries: imbl::OrdMap<Box<[u8]>, Arc<[u8]>>,
    bytes: u64,
}

impl PartialEq for ControlState {
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq && self.follows == other.follows && self.entries == other.entries
    }
}

impl ControlState {
    pub fn get(&self, key: &[u8]) -> Option<&Arc<[u8]>> {
        self.entries.get(key)
    }

    /// The entries whose keys start with `prefix`, in key order.
    pub fn prefix<'a>(
        &'a self,
        prefix: &'a [u8],
    ) -> impl Iterator<Item = (&'a Box<[u8]>, &'a Arc<[u8]>)> + 'a {
        let start: Box<[u8]> = prefix.into();
        self.entries
            .range(start..)
            .take_while(move |(k, _)| k.starts_with(prefix))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes of the keys and values, which `maxControlBytes` bounds.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The origin entry, when the state carries one.
    pub fn origin(&self) -> Result<Option<Origin>> {
        self.get(ORIGIN_KEY)
            .map(|v| serde_json::from_slice(v).map_err(|e| corrupt(format!("origin entry: {e}"))))
            .transpose()
    }

    fn put(&mut self, key: Box<[u8]>, value: Arc<[u8]>) {
        let add = (key.len() + value.len()) as u64;
        if let Some(old) = self.entries.insert(key.clone(), value) {
            self.bytes -= (key.len() + old.len()) as u64;
        }
        self.bytes += add;
    }

    fn delete(&mut self, key: &[u8]) {
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= (key.len() + old.len()) as u64;
        }
    }

    /// This state with `frame` applied. The frame must be the next one.
    pub fn apply(&self, frame: &Frame) -> Result<ControlState> {
        if frame.seq != self.seq + 1 {
            return Err(corrupt(format!(
                "frame {} follows frame {}",
                frame.seq, self.seq
            )));
        }
        let mut next = self.clone();
        for op in &frame.ops {
            match op {
                Op::Put(k, v) => next.put(k.clone(), v.clone()),
                Op::Delete(k) => next.delete(k),
            }
        }
        next.seq = frame.seq;
        next.follows = frame.follows;
        Ok(next)
    }

    /// This state with `key` set to `value`, or removed, at the same sequence number, as
    /// a capture renders its origin entry.
    pub fn with_entry(&self, key: &[u8], value: Option<Arc<[u8]>>) -> ControlState {
        let mut next = self.clone();
        match value {
            Some(v) => next.put(key.into(), v),
            None => next.delete(key),
        }
        next
    }
}

// ------------------------------------------------------------------ checkpoint ------

/// The bytes of `control.bin` for `state`.
pub fn encode_checkpoint(state: &ControlState) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + state.bytes as usize + state.len() * 6 + 32);
    out.extend_from_slice(&CHECKPOINT_MAGIC);
    out.extend_from_slice(&CHECKPOINT_VERSION.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&state.seq.to_le_bytes());
    out.extend_from_slice(&state.follows.to_le_bytes());
    out.extend_from_slice(&(state.len() as u64).to_le_bytes());
    for (k, v) in state.entries.iter() {
        out.extend_from_slice(&(k.len() as u16).to_le_bytes());
        out.extend_from_slice(k);
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v);
    }
    let digest = sha2::Sha256::digest(&out);
    out.extend_from_slice(&digest);
    out
}

/// The state `control.bin` holds. A version this build does not read is refused with
/// [`Error::Unsupported`] before anything after it is read.
pub fn decode_checkpoint(bytes: &[u8]) -> Result<ControlState> {
    if bytes.len() < 16 || bytes[..8] != CHECKPOINT_MAGIC {
        return Err(corrupt(format!("{CHECKPOINT_FILE} has no header")));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    crate::formats::CONTROL_CHECKPOINT.check(CHECKPOINT_FILE, version, None)?;
    if bytes.len() < 40 + 32 {
        return Err(corrupt(format!("{CHECKPOINT_FILE} ends early")));
    }
    let (body, digest) = bytes.split_at(bytes.len() - 32);
    if sha2::Sha256::digest(body).as_slice() != digest {
        return Err(corrupt(format!("{CHECKPOINT_FILE} fails its digest")));
    }
    let mut r = Reader {
        bytes: body,
        at: 16,
    };
    let mut state = ControlState {
        seq: r.u64()?,
        follows: r.u64()?,
        ..Default::default()
    };
    let count = r.u64()?;
    let mut last: Option<Box<[u8]>> = None;
    for _ in 0..count {
        let klen = r.u16()? as usize;
        let key: Box<[u8]> = r.take(klen)?.into();
        let vlen = r.u32()? as usize;
        let value: Arc<[u8]> = r.take(vlen)?.into();
        check_entry(&key, Some(&value)).map_err(|e| corrupt(e))?;
        if last.as_ref().is_some_and(|l| *l >= key) {
            return Err(corrupt(format!("{CHECKPOINT_FILE} keys are out of order")));
        }
        last = Some(key.clone());
        state.put(key, value);
    }
    if r.at != body.len() {
        return Err(corrupt(format!(
            "{CHECKPOINT_FILE} has bytes after its entries"
        )));
    }
    Ok(state)
}

// ------------------------------------------------------------------------- log ------

/// The header of a new `control.log`.
pub fn log_header() -> [u8; LOG_HEADER_BYTES as usize] {
    let mut h = [0; LOG_HEADER_BYTES as usize];
    h[..8].copy_from_slice(&LOG_MAGIC);
    h[8..12].copy_from_slice(&LOG_VERSION.to_le_bytes());
    h
}

/// The record of `control.log` that holds the encoded `frame`.
pub fn log_record(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + frame.len());
    out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    out.extend_from_slice(&crate::commit::crc32_of(&[frame]).to_le_bytes());
    out.extend_from_slice(frame);
    out
}

/// What [`read_log`] found.
#[derive(Debug)]
pub struct LogRead {
    pub frames: Vec<Frame>,
    /// the bytes of the header and the complete records, where a writable open
    /// truncates the file
    pub good: u64,
    /// whether a damaged last record follows them
    pub torn: bool,
}

/// The frames of `control.log`. A damaged last record is a torn tail. A damaged
/// record with bytes other than zeros after it is corruption (spec C26 §7.2). A header
/// of another version is refused before any record is read.
pub fn read_log(bytes: &[u8]) -> Result<LogRead> {
    if bytes.len() < LOG_HEADER_BYTES as usize || bytes[..8] != LOG_MAGIC {
        return Err(corrupt(format!("{LOG_FILE} has no header")));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    crate::formats::CONTROL_LOG.check(LOG_FILE, version, None)?;
    if bytes[12..16] != [0; 4] {
        return Err(corrupt(format!(
            "{LOG_FILE} header has non-zero reserved bytes"
        )));
    }
    let mut at = LOG_HEADER_BYTES as usize;
    let mut frames = Vec::new();
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (len, crc) = if rest.len() >= 8 {
            (
                u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize,
                u32::from_le_bytes(rest[4..8].try_into().unwrap()),
            )
        } else {
            (usize::MAX, 0)
        };
        let end = at.saturating_add(8).saturating_add(len);
        let complete = len <= MAX_FRAME && end <= bytes.len();
        let valid = complete && len > 0 && crate::commit::crc32_of(&[&bytes[at + 8..end]]) == crc;
        if !valid {
            // Every record is synchronized before the next is written, so damage before
            // a later record is not a torn tail. Only zeros may follow a complete
            // damaged record, and no complete record may follow one whose damaged
            // length runs past the end.
            let after = if complete { end } else { bytes.len() };
            if bytes[after..].iter().any(|b| *b != 0) || later_record(bytes, at + 1) {
                return Err(corrupt(format!(
                    "{LOG_FILE} has a damaged record at byte {at} before later records"
                )));
            }
            return Ok(LogRead {
                frames,
                good: at as u64,
                torn: true,
            });
        }
        frames.push(Frame::decode(&bytes[at + 8..end])?);
        at = end;
    }
    Ok(LogRead {
        frames,
        good: at as u64,
        torn: false,
    })
}

/// Whether a complete, valid record of `control.log` starts at or after `from`: one whose
/// frame starts with the frame version and declares the record's length, and whose
/// checksum matches. The first two tests leave the checksum to few candidates.
fn later_record(bytes: &[u8], from: usize) -> bool {
    let mut p = from;
    while p + 8 + 5 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        let end = p.saturating_add(8).saturating_add(len);
        if len > 5
            && len <= MAX_FRAME
            && end <= bytes.len()
            && bytes[p + 8] == FRAME_VERSION
            && u32::from_le_bytes(bytes[p + 9..p + 13].try_into().unwrap()) as usize == len
            && crate::commit::crc32_of(&[&bytes[p + 8..end]])
                == u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap())
        {
            return true;
        }
        p += 1;
    }
    false
}

// -------------------------------------------------------------------- recovery ------

/// A frame of the write-ahead log and the commit that carries it.
#[derive(Clone, Debug)]
pub struct WalFrame {
    pub commit: u64,
    pub frame: Frame,
}

/// The control state at open: `checkpoint`, then every frame above its sequence number
/// from the write-ahead log (`wal`, the transactions the replay keeps) and from
/// `control.log` (`log`), in sequence order. `head` is the recovered business head.
/// Every contradiction of spec C26 §7.1 is an integrity error.
pub fn recover(
    checkpoint: ControlState,
    wal: Vec<WalFrame>,
    log: &[Frame],
    head: u64,
) -> Result<ControlState> {
    if checkpoint.follows > head {
        return Err(corrupt(format!(
            "{CHECKPOINT_FILE} follows commit {} above the recovered head {head}",
            checkpoint.follows
        )));
    }
    let mut frames: Vec<&Frame> = Vec::with_capacity(wal.len() + log.len());
    for w in &wal {
        if w.frame.follows != w.commit {
            return Err(corrupt(format!(
                "commit {} carries a frame that names commit {}",
                w.commit, w.frame.follows
            )));
        }
        if w.frame.seq > checkpoint.seq {
            frames.push(&w.frame);
        }
    }
    for f in log {
        if f.follows > head {
            return Err(corrupt(format!(
                "control frame {} follows commit {} above the recovered head {head}",
                f.seq, f.follows
            )));
        }
        if f.seq > checkpoint.seq {
            frames.push(f);
        }
    }
    frames.sort_by_key(|f| f.seq);
    let mut state = checkpoint;
    for f in frames {
        if f.seq <= state.seq {
            return Err(corrupt(format!("control frame {} appears twice", f.seq)));
        }
        if f.seq != state.seq + 1 {
            return Err(corrupt(format!(
                "control frames {} to {} are missing",
                state.seq + 1,
                f.seq - 1
            )));
        }
        if f.follows < state.follows {
            return Err(corrupt(format!(
                "control frame {} follows commit {} before commit {}",
                f.seq, f.follows, state.follows
            )));
        }
        state = state.apply(f)?;
    }
    Ok(state)
}

/// Check the control closure of a restored dataset at `root` before it is reidentified
/// or published, whether or not an integrity check was asked for (spec C26 §7.3). The
/// feature `control/1` (`feature`), a `jobs.json`, a jobs entry in the registry and a
/// current log of version 3 each require a `control.bin`, which must carry an origin
/// entry that names the backup's source dataset `dataset` and its captured commit
/// `commit`, also the checkpoint's business head. A `control.log` is refused.
pub fn check_restored(root: &Path, feature: bool, dataset: uuid::Uuid, commit: u64) -> Result<()> {
    let refuse = |why: String| {
        Err(Error::Corrupt(format!(
            "the restored control keyspace: {why}"
        )))
    };
    if root.join(LOG_FILE).exists() {
        return refuse(format!("a backup never holds {LOG_FILE}"));
    }
    let jobs = root.join(crate::jobs::FILE).exists();
    let registry = match std::fs::read(root.join(crate::managed::FILE)) {
        Ok(bytes) => crate::managed::recorded_owners(&bytes)
            .map_err(|e| Error::Corrupt(format!("the restored registry: {e}")))?
            .contains(&crate::managed::OwnerKind::Jobs),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    let current = std::fs::read_to_string(root.join("CURRENT"))?;
    let wal = root.join(current.trim()).join("wal.log");
    let version3 = wal.exists()
        && crate::store::wal::file_version(&wal)? == crate::formats::WAL_CONTROL_VERSION;
    let bin = match std::fs::read(root.join(CHECKPOINT_FILE)) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let Some(bin) = bin else {
        if feature || jobs || registry || version3 {
            return refuse(format!(
                "the backup needs {CHECKPOINT_FILE}, which it does not hold"
            ));
        }
        return Ok(());
    };
    if !feature {
        return refuse(format!("{CHECKPOINT_FILE} needs the feature control/1"));
    }
    let state = decode_checkpoint(&bin)?;
    match state.origin()? {
        Some(o)
            if o.kind == "capture"
                && o.dataset == dataset
                && o.commit == commit
                && state.follows == commit =>
        {
            Ok(())
        }
        Some(_) => refuse(format!(
            "the origin of {CHECKPOINT_FILE} does not name dataset {dataset} at commit {commit}"
        )),
        None => refuse(format!("{CHECKPOINT_FILE} carries no origin entry")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, follows: u64, ops: Vec<Op>) -> Frame {
        Frame {
            seq,
            follows,
            time_ms: 7,
            ops,
        }
    }

    fn put(k: &str, v: &str) -> Op {
        Op::Put(k.as_bytes().into(), v.as_bytes().into())
    }

    #[test]
    fn frames_round_trip_through_bytes_and_wal_chunks() {
        let f = frame(
            3,
            9,
            vec![
                put("jobs/run/a", "{}"),
                Op::Delete(b"jobs/x".as_slice().into()),
            ],
        );
        let bytes = f.encode().unwrap();
        assert_eq!(Frame::decode(&bytes).unwrap(), f);
        let payload: Vec<u8> = chunks(&bytes).flatten().collect();
        assert_eq!(payload.len() % CHUNK, 0);
        assert_eq!(Frame::from_chunks(&payload).unwrap(), f);
        // a non-zero padding byte, a whole extra chunk and a newer version are refused
        let mut bad = payload.clone();
        *bad.last_mut().unwrap() = 1;
        if bytes.len() % CHUNK != 0 {
            assert!(Frame::from_chunks(&bad).is_err());
        }
        let mut extra = payload.clone();
        extra.extend_from_slice(&[0; CHUNK]);
        assert!(Frame::from_chunks(&extra).is_err());
        let mut newer = bytes.clone();
        newer[0] = 2;
        assert!(matches!(Frame::decode(&newer), Err(Error::Unsupported(_))));
    }

    #[test]
    fn entry_limits_are_enforced() {
        let long = vec![b'k'; MAX_KEY + 1];
        let f = frame(1, 0, vec![Op::Put(long.into(), Arc::from(&b""[..]))]);
        assert!(f.encode().is_err());
    }

    #[test]
    fn checkpoints_round_trip_and_refuse_damage_and_newer_versions() {
        let s = ControlState::default()
            .apply(&frame(1, 4, vec![put("b", "2"), put("a", "1")]))
            .unwrap();
        let bytes = encode_checkpoint(&s);
        let back = decode_checkpoint(&bytes).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.bytes(), 4);
        let mut damaged = bytes.clone();
        damaged[30] ^= 1;
        assert!(matches!(
            decode_checkpoint(&damaged),
            Err(Error::Corrupt(_))
        ));
        let mut newer = bytes.clone();
        newer[8] = 2;
        assert!(matches!(
            decode_checkpoint(&newer),
            Err(Error::Unsupported(_))
        ));
    }

    fn log_of(frames: &[Frame]) -> Vec<u8> {
        let mut out = log_header().to_vec();
        for f in frames {
            out.extend(log_record(&f.encode().unwrap()));
        }
        out
    }

    #[test]
    fn the_log_reads_its_frames_and_a_torn_tail() {
        let fs = [
            frame(1, 0, vec![put("a", "1")]),
            frame(2, 0, vec![put("b", "2")]),
        ];
        let bytes = log_of(&fs);
        let r = read_log(&bytes).unwrap();
        assert_eq!(r.frames, fs);
        assert_eq!(r.good, bytes.len() as u64);
        assert!(!r.torn);
        // a record cut short is a torn tail
        let cut = &bytes[..bytes.len() - 3];
        let r = read_log(cut).unwrap();
        assert_eq!(r.frames.len(), 1);
        assert!(r.torn);
        // zeros after the last record are a torn tail too
        let mut zeros = bytes.clone();
        zeros.extend_from_slice(&[0; 40]);
        let r = read_log(&zeros).unwrap();
        assert_eq!(r.frames.len(), 2);
        assert!(r.torn);
        assert_eq!(r.good, bytes.len() as u64);
        // a damaged record before a later one is corruption
        let first_end = LOG_HEADER_BYTES as usize + 8 + fs[0].encode().unwrap().len();
        let mut mid = bytes.clone();
        mid[first_end - 1] ^= 1;
        assert!(matches!(read_log(&mid), Err(Error::Corrupt(_))));
        // so is a damaged length that runs past the end over a later valid record
        let h = LOG_HEADER_BYTES as usize;
        let mut long = bytes.clone();
        long[h..h + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(read_log(&long), Err(Error::Corrupt(_))));
        let mut past = bytes.clone();
        past[h..h + 4].copy_from_slice(&((bytes.len() - h) as u32).to_le_bytes());
        assert!(matches!(read_log(&past), Err(Error::Corrupt(_))));
        // a newer version is refused
        let mut newer = bytes.clone();
        newer[8] = 2;
        assert!(matches!(read_log(&newer), Err(Error::Unsupported(_))));
    }

    #[test]
    fn recovery_merges_both_logs_and_refuses_contradictions() {
        let cp = ControlState::default()
            .apply(&frame(1, 1, vec![put("a", "1")]))
            .unwrap();
        let wal = vec![
            WalFrame {
                commit: 1,
                frame: frame(1, 1, vec![put("a", "1")]),
            },
            WalFrame {
                commit: 3,
                frame: frame(3, 3, vec![put("c", "3")]),
            },
        ];
        let log = vec![
            frame(2, 2, vec![put("b", "2")]),
            frame(4, 3, vec![Op::Delete(b"a".as_slice().into())]),
        ];
        let s = recover(cp.clone(), wal.clone(), &log, 3).unwrap();
        assert_eq!(s.seq, 4);
        assert_eq!(s.get(b"a"), None);
        assert_eq!(s.get(b"c").map(|v| &v[..]), Some(&b"3"[..]));
        // a gap
        assert!(recover(cp.clone(), wal.clone(), &log[1..], 3).is_err());
        // a repeated number
        let mut twice = log.clone();
        twice.push(frame(3, 3, vec![]));
        assert!(recover(cp.clone(), wal.clone(), &twice, 3).is_err());
        // a control-only frame above the head, and a checkpoint above the head
        assert!(recover(cp.clone(), wal.clone(), &log, 2).is_err());
        let mut high = cp.clone();
        high.follows = 9;
        assert!(recover(high, Vec::new(), &[], 3).is_err());
        // a WAL frame that names another commit
        let wrong = vec![WalFrame {
            commit: 2,
            frame: frame(2, 1, vec![]),
        }];
        assert!(recover(cp, wrong, &[], 3).is_err());
    }
}
