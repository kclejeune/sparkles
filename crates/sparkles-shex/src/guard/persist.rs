//! The typing of the head between runs (`validation-typing.bin`).
//!
//! A schema whose references follow arcs keeps the typing of the head, so a write
//! validates only the pairs it changes. The typing lives in memory, and a restart used to
//! throw it away: the first write after it validated the whole dataset to rebuild it.
//! The guard now writes the typing when the store closes and reads it back when the
//! guard is installed, so that first write is as cheap as the others. The file costs
//! memory only while it is read (its bytes are freed once the typing is rebuilt) and
//! disk space about the size of the typing.
//!
//! The file is trusted only for the head it was written for. Its header names the dataset,
//! the head commit (sequence number, time and quad count), the configuration the guard
//! runs with (the SHA-256 of `validation.json`, which also names the schema copy's
//! hash) and a fingerprint of the compiled schema's pair kinds that also covers the
//! SHA-256 of the schema copy, so an edited copy never takes the typing of the schema it
//! replaced. A SHA-256 of the whole file ends it. That digest detects damage and not
//! tampering. The file is trusted like the rest of the database directory, because anyone
//! who can write it can also write `validation.json` and turn the guard off. Anything
//! that does not match leaves the typing unknown, and the first write validates in full
//! as before. The first commit after an open removes the file, so a head that moved on
//! can never be mistaken for the one in the file: it is written again on a clean close,
//! and only then. A crash leaves no file or a stale one that is never trusted.
//!
//! Nodes are keyed by term and never by id, since ids differ between generations:
//! stored terms by their vocabulary key, inline values and blank nodes by their id (a
//! blank node's id is derived from its label). A compaction renumbers the ids of the
//! typing in memory instead ([`rekey`]).

use super::{HeadTyping, ShexGuard};
use crate::engine::PairValues;
use crate::ir::{Ir, PairKind};
use crate::typing::Verdict;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use sparkles_core::commit::CommitInfo;
use sparkles_core::formats::VALIDATION_TYPING;
use sparkles_core::guard::config::TYPING_FILE;
use sparkles_core::id::{Id, Tag};
use sparkles_core::store::Snapshot;
use std::io::Write;
use std::path::Path;

const MAGIC: &[u8; 8] = b"SPXTYP01";

/// Version of the layout, registered as [`VALIDATION_TYPING`]. A change to it, or to the
/// numbering of pair kinds, goes with a new number (the crate version is part of the
/// fingerprint as well).
const TYPING_FORMAT: u32 = VALIDATION_TYPING.writes;

/// Node ids written as they are: inline values and blank nodes.
const NODE_RAW: u8 = 0;
/// Node ids written as the vocabulary key of the term they stand for.
const NODE_KEY: u8 = 1;

/// The file's identity checks: what the typing is the typing of.
pub(super) struct Header<'a> {
    pub dataset: [u8; 16],
    pub commit: u64,
    pub timestamp_ms: i64,
    pub quads: u64,
    pub config_hash: &'a str,
    pub fingerprint: [u8; 32],
}

/// A digest of what the numbers of pair kinds mean, and of the schema they come from: two
/// compilers that number them differently, and two schemas that differ in any constraint,
/// give different fingerprints. `schema_digest` is the SHA-256 of the schema copy.
pub(super) fn fingerprint(ir: &Ir, schema_digest: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(TYPING_FORMAT.to_le_bytes());
    h.update((ir.pairs.len() as u64).to_le_bytes());
    for p in &ir.pairs {
        h.update((p.se.index() as u64).to_le_bytes());
        h.update(p.stratum.to_le_bytes());
        match &p.label {
            Some(l) => {
                h.update([1]);
                h.update(l.to_shexj().as_bytes());
            }
            None => h.update([0]),
        }
        h.update([0xff]);
    }
    h.update(schema_digest.as_bytes());
    h.finalize().into()
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A writer that hashes what passes through it.
struct Hashing<W: Write> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.hash.update(&b[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Write the typing of `snap` (the head) to `root`, replacing the file by a rename (not
/// synced: a missing or stale file only makes the typing unknown). Nodes that stand for
/// no stored term, and the typing's local ids, are left out. They have no arcs, so no
/// pair reads their pairs.
pub(super) fn write(
    root: &Path,
    snap: &Snapshot,
    t: &HeadTyping,
    head: &Header<'_>,
) -> std::io::Result<()> {
    let tmp = root.join(format!("{TYPING_FILE}.tmp"));
    let file = std::fs::File::create(&tmp)?;
    let mut w = Hashing {
        inner: std::io::BufWriter::with_capacity(1 << 20, file),
        hash: Sha256::new(),
    };
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&TYPING_FORMAT.to_le_bytes());
    buf.extend_from_slice(&head.dataset);
    buf.extend_from_slice(&head.commit.to_le_bytes());
    buf.extend_from_slice(&head.timestamp_ms.to_le_bytes());
    buf.extend_from_slice(&head.quads.to_le_bytes());
    varint(&mut buf, head.config_hash.len() as u64);
    buf.extend_from_slice(head.config_hash.as_bytes());
    buf.extend_from_slice(&head.fingerprint);
    // nodes with something to write, in id order
    let mut nodes: Vec<(&Id, &Vec<PairKind>)> = t
        .by_node
        .iter()
        .filter(|(n, _)| n.tag() != Tag::Local && n.tag() != Tag::Undef)
        .collect();
    nodes.sort_unstable_by_key(|(n, _)| **n);
    let mut kept: Vec<(u8, Vec<u8>, &Id)> = Vec::with_capacity(nodes.len());
    for &(n, _) in &nodes {
        match n.tag() {
            Tag::Vocab | Tag::Delta => {
                if let Some(k) = snap.key(*n) {
                    kept.push((NODE_KEY, k.into_owned(), n));
                }
            }
            _ => kept.push((NODE_RAW, n.0.to_le_bytes().to_vec(), n)),
        }
    }
    varint(&mut buf, kept.len() as u64);
    w.write_all(&buf)?;
    for (kind, bytes, n) in kept {
        buf.clear();
        buf.push(kind);
        if kind == NODE_KEY {
            varint(&mut buf, bytes.len() as u64);
        }
        buf.extend_from_slice(&bytes);
        let mut pairs: Vec<(u32, u8)> = t.by_node[n]
            .iter()
            .filter_map(|&k| {
                let v = t.values.get(&(*n, k))?;
                Some((
                    k.index() as u32,
                    match v {
                        Verdict::False => 0,
                        Verdict::True => 1,
                        Verdict::AlwaysFalse => 2,
                    },
                ))
            })
            .collect();
        pairs.sort_unstable();
        varint(&mut buf, pairs.len() as u64);
        for (k, v) in pairs {
            varint(&mut buf, k as u64);
            buf.push(v);
        }
        w.write_all(&buf)?;
    }
    let digest = w.hash.clone().finalize();
    w.inner.write_all(&digest)?;
    w.inner.flush()?;
    drop(w);
    std::fs::rename(&tmp, root.join(TYPING_FILE))
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn varint(&mut self) -> Option<u64> {
        let (mut v, mut shift) = (0u64, 0u32);
        loop {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }
}

/// The typing in the file of `root`, if it is the typing of the head `head` names (with
/// the pair kinds of `pairs` kinds) read in `snap`. A file that does not parse, that is
/// of another head or configuration, or that fails its digest, is removed.
pub(super) fn read(
    root: &Path,
    snap: &Snapshot,
    head: &Header<'_>,
    pairs: usize,
) -> Option<HeadTyping> {
    let path = root.join(TYPING_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return None,
    };
    let t = parse(&bytes, snap, head, pairs);
    drop(bytes);
    if t.is_none() {
        let _ = std::fs::remove_file(&path);
    }
    t
}

fn parse(bytes: &[u8], snap: &Snapshot, head: &Header<'_>, pairs: usize) -> Option<HeadTyping> {
    // the version comes first: a layout this build does not know cannot be checked by its
    // trailing digest, and is a rebuild and not damage
    if bytes.get(..8)? != MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
    if !VALIDATION_TYPING.reads(version) {
        tracing::info!(
            "{TYPING_FILE} has version {version}; this build reads version {}, so the typing is rebuilt by a full validation",
            VALIDATION_TYPING.readable()
        );
        return None;
    }
    let body = bytes.len().checked_sub(32)?;
    let (data, digest) = bytes.split_at(body);
    if Sha256::digest(data).as_slice() != digest {
        return None;
    }
    let mut c = Cursor { b: data, at: 0 };
    if c.take(8)? != MAGIC || c.u32()? != version || c.take(16)? != head.dataset {
        return None;
    }
    if c.u64()? != head.commit || c.u64()? as i64 != head.timestamp_ms || c.u64()? != head.quads {
        return None;
    }
    let n = c.varint()? as usize;
    if c.take(n)? != head.config_hash.as_bytes() {
        return None;
    }
    if c.take(32)? != head.fingerprint {
        return None;
    }
    let nodes = c.varint()? as usize;
    let mut values: PairValues = Vec::new();
    for _ in 0..nodes {
        let id = match c.u8()? {
            NODE_RAW => {
                let id = Id(c.u64()?);
                if !matches!(
                    id.tag(),
                    Tag::BNode
                        | Tag::Bool
                        | Tag::Int
                        | Tag::Double
                        | Tag::Decimal
                        | Tag::DateTime
                        | Tag::Date
                ) {
                    return None;
                }
                Some(id)
            }
            NODE_KEY => {
                let len = c.varint()? as usize;
                // a term the store no longer has has no arcs: its pairs are dropped
                snap.lookup_key(c.take(len)?)
            }
            _ => return None,
        };
        let count = c.varint()? as usize;
        for _ in 0..count {
            let k = c.varint()? as usize;
            let v = match c.u8()? {
                0 => Verdict::False,
                1 => Verdict::True,
                2 => Verdict::AlwaysFalse,
                _ => return None,
            };
            if k >= pairs {
                return None;
            }
            if let Some(id) = id {
                values.push(((id, PairKind(k as u32)), v));
            }
        }
    }
    (c.at == data.len()).then(|| HeadTyping::new(head.commit, snap.generation.uid, values))
}

/// The typing translated to the ids of `new`, which holds the data of `old` renumbered
/// (a compaction); `None` when it is not the typing of `old`.
pub(super) fn translate(t: &HeadTyping, old: &Snapshot, new: &Snapshot) -> Option<HeadTyping> {
    if t.generation != old.generation.uid || t.commit != new.commit {
        return None;
    }
    let mut map: FxHashMap<Id, Option<Id>> = FxHashMap::default();
    let mut values: PairValues = Vec::with_capacity(t.values.len());
    for (&(n, k), &v) in &t.values {
        let to = *map.entry(n).or_insert_with(|| match n.tag() {
            Tag::Vocab | Tag::Delta => {
                let key = old.key(n)?;
                new.lookup_key(&key)
            }
            Tag::Local | Tag::Undef | Tag::Special => None,
            _ => Some(n),
        });
        if let Some(to) = to {
            values.push(((to, k), v));
        }
    }
    Some(HeadTyping::new(t.commit, new.generation.uid, values))
}

impl ShexGuard {
    /// Whether the typing is kept in a file: a persistent dataset whose schema is a copy
    /// in its directory.
    pub(super) fn persists_typing(&self) -> bool {
        self.persist.is_some() && self.cfg.schema.graphs.is_none()
    }

    fn header<'a>(&self, snap: &Snapshot, head: &CommitInfo, hash: &'a str) -> Header<'a> {
        Header {
            dataset: *snap.dataset_id.as_bytes(),
            commit: head.seq,
            timestamp_ms: head.timestamp_ms,
            quads: head.quads,
            config_hash: hash,
            fingerprint: {
                let model = self.model();
                fingerprint(model.schema.ir(), &model.digest)
            },
        }
    }

    /// Read the typing of the head from its file (see the module documentation), and
    /// remember that the file is there.
    pub(super) fn restore_typing(&self, snap: &Snapshot, head: &CommitInfo) {
        let Some((root, hash)) = self.persist.as_ref().filter(|_| self.persists_typing()) else {
            return;
        };
        let model = self.model();
        if !model.keeps_typing() || snap.commit != head.seq {
            return;
        }
        let h = self.header(snap, head, hash);
        if let Some(t) = read(root, snap, &h, model.schema.ir().pairs.len()) {
            *self.saved.lock() = Some(t.commit);
            *self.typing.lock() = Some(t);
        }
    }

    /// The commit after a read of the file moved the head on (or a write bypassed the
    /// guard): the file is no longer the typing of the head, and goes.
    pub(super) fn forget_saved_typing(&self) {
        let mut saved = self.saved.lock();
        if saved.take().is_some()
            && let Some((root, _)) = &self.persist
            && let Err(e) = std::fs::remove_file(root.join(TYPING_FILE))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("cannot remove {TYPING_FILE}: {e}");
        }
    }

    /// The clean close of the store: keep the typing of the head, unless the file
    /// already is it.
    pub(super) fn save_typing(&self, head: &CommitInfo, snap: &Snapshot) {
        let Some((root, hash)) = self.persist.as_ref().filter(|_| self.persists_typing()) else {
            return;
        };
        let typing = self.typing.lock();
        let Some(t) = typing
            .as_ref()
            .filter(|t| t.commit == head.seq && t.generation == snap.generation.uid)
        else {
            return;
        };
        let mut saved = self.saved.lock();
        if *saved == Some(head.seq) {
            return;
        }
        let h = self.header(snap, head, hash);
        match write(root, snap, t, &h) {
            Ok(()) => *saved = Some(head.seq),
            Err(e) => {
                tracing::warn!("cannot write {TYPING_FILE}: {e}");
                let _ = std::fs::remove_file(root.join(format!("{TYPING_FILE}.tmp")));
            }
        }
    }

    /// A compaction renumbered the terms: translate the typing, or forget it.
    pub(super) fn rekey_typing(&self, old: &Snapshot, new: &Snapshot) {
        let mut typing = self.typing.lock();
        if let Some(t) = typing.as_ref() {
            *typing = translate(t, old, new);
        }
    }
}
