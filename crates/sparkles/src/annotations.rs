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
//! A record is appended and synced *before* the commit point (the WAL fsync, or the
//! `CURRENT` switch of a bulk commit), so an acknowledged commit always has its
//! annotation. A record whose commit never became durable has a `seq` above the head
//! found at the next open, which truncates it away. A torn or damaged tail is truncated
//! too. In-memory stores keep annotations in memory only.

use crate::commit::{CommitInfo, rfc3339_ms};
use crate::error::{Error, Result};
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
const FLAG_DIGESTS: u32 = 1;
const REC_MESSAGE: u8 = 1;
const REC_DIGEST: u8 = 2;

/// Longest commit message, in bytes of UTF-8.
pub const MAX_MESSAGE_BYTES: usize = 1024;

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
    file: Option<File>,
    /// bytes of the file that hold complete, valid records
    len: u64,
    map: BTreeMap<u64, Annotation>,
    /// compute change digests for new commits
    digests: bool,
}

impl Annotations {
    pub fn memory(digests: bool) -> Annotations {
        Annotations {
            path: None,
            file: None,
            len: 0,
            map: BTreeMap::new(),
            digests,
        }
    }

    /// Open (or create) `<root>/annotations.bin`, keeping the records of commits up to
    /// `head`. `digests` turns digests on; once on, they stay on (the header flag).
    pub fn open(
        root: &Path,
        dataset_id: uuid::Uuid,
        head: u64,
        digests: bool,
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
        let mut file = None;
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
            file = Some(f);
        }
        Ok(Annotations {
            path: Some(path),
            file,
            len,
            map,
            digests,
        })
    }

    pub fn digests(&self) -> bool {
        self.digests
    }

    pub fn get(&self, seq: u64) -> Option<&Annotation> {
        self.map.get(&seq)
    }

    /// Record `a` for commit `seq` (the next commit, not yet durable) and, for a
    /// persistent store, sync it to disk. Undo it with [`Annotations::undo`] if the
    /// commit does not happen.
    pub fn append(&mut self, seq: u64, a: Annotation, dataset_id: uuid::Uuid) -> Result<()> {
        if a.is_empty() {
            return Ok(());
        }
        if let Some(path) = &self.path {
            if self.file.is_none() {
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
                self.file = Some(f);
            }
            let rec = encode_record(seq, &a);
            let f = self.file.as_mut().expect("opened above");
            use std::io::{Seek, SeekFrom};
            let written = f
                .seek(SeekFrom::Start(self.len))
                .and_then(|_| f.write_all(&rec))
                .and_then(|_| f.sync_data());
            if let Err(e) = written {
                // leave no partial record behind
                let _ = f.set_len(self.len);
                return Err(e.into());
            }
            self.len += rec.len() as u64;
        }
        self.map.insert(seq, a);
        Ok(())
    }

    /// Take back the annotation of `seq`, appended for a commit that did not happen.
    /// `Err` when the file could not be cut back (the caller refuses further writes).
    pub fn undo(&mut self, seq: u64) -> Result<()> {
        let Some(a) = self.map.remove(&seq) else {
            return Ok(());
        };
        if let Some(f) = self.file.as_mut() {
            let n = encode_record(seq, &a).len() as u64;
            self.len -= n;
            f.set_len(self.len)?;
            f.sync_data()?;
        }
        Ok(())
    }

    /// In-memory stores forget the annotations of commits the catalog no longer has.
    pub fn forget_before(&mut self, seq: u64) {
        if self.path.is_none() {
            self.map = self.map.split_off(&seq);
        }
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

fn decode_record(buf: &[u8], start: usize) -> Option<(u64, Annotation, usize)> {
    let mut pos = start;
    let take = |pos: &mut usize, n: usize| -> Option<&[u8]> {
        let s = buf.get(*pos..*pos + n)?;
        *pos += n;
        Some(s)
    };
    let seq = u64::from_le_bytes(take(&mut pos, 8)?.try_into().ok()?);
    let flags = take(&mut pos, 1)?[0];
    if flags & !(REC_MESSAGE | REC_DIGEST) != 0 {
        return None;
    }
    let mut a = Annotation::default();
    if flags & REC_DIGEST != 0 {
        a.digest = Some(take(&mut pos, 32)?.try_into().ok()?);
    }
    if flags & REC_MESSAGE != 0 {
        let n = u16::from_le_bytes(take(&mut pos, 2)?.try_into().ok()?) as usize;
        let m = std::str::from_utf8(take(&mut pos, n)?).ok()?;
        a.message = Some(Arc::from(m));
    }
    let body_end = pos;
    let crc = u32::from_le_bytes(take(&mut pos, 4)?.try_into().ok()?);
    let mut c = flate2::Crc::new();
    c.update(&buf[start..body_end]);
    (c.sum() == crc).then_some((seq, a, pos))
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
}
