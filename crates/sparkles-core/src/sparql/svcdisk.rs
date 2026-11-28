//! The disk tier of the cache of remote SERVICE results (spec G10, roadmap item 38).
//!
//! A persistent dataset whose `service-cache.json` sets `disk` keeps the entries of its
//! SERVICE cache in `service-cache/` under its directory too, so that they outlive the
//! process. Each entry is one file, named by the SHA-256 of its key. The key holds the
//! caller's principal and any user name and password of the endpoint's URL, so it is
//! never written. The file holds the digest of the key and the digest of the scope,
//! which a lookup and a listing compare, and the endpoint as `se:cacheLs` lists it.
//!
//! A file starts with the magic `SPKSVC\0\0`, its version as a little-endian u32, the
//! length of its body as a u64 and the SHA-256 of the body. The body holds, in order,
//! the time the entry was stored in seconds since the Unix epoch, the key digest, the
//! scope digest, the lengths of the key's scope, endpoint and pattern, the offset and the
//! limit of the request's slice, with `u64::MAX` for no limit, the endpoint, the pattern,
//! the values, the variable names and the rows. Numbers are little-endian. A string is a u32 length
//! and its UTF-8 bytes, and a term is a string of its N-Triples text, with the length
//! `u32::MAX` for an unbound one.
//!
//! A file is written under a temporary name and renamed, so a reader sees all of a file
//! or none of it. A body that does not match its digest, which a crash in the middle of a
//! write leaves, is a miss and is removed, and so is a file without the magic and one
//! whose key or scope digest is not the one its name and the request call for. A file of
//! another version is a miss too, but it is kept and not overwritten, since a newer
//! release may read it, and it counts toward the cap and is evicted like any other. Every
//! length is checked against the bytes left before anything is allocated.
//!
//! The tier's index and its files change together under one lock, so a write, a removal
//! and an eviction never interleave, and a file that cannot be removed stays in the index
//! and is not counted as removed. A read takes the file's generation from the index
//! before it reads the file and answers only if the file is still that generation after,
//! so an entry removed or replaced while it was read is a miss. The removals that
//! `se:cacheRm`, a clear and turning the tier off make sync the directory before they
//! return, so a removal that was reported is not undone by a crash.
//!
//! The files together are bounded by the tier's cap and the least recently used ones are
//! evicted first. An entry larger than an eighth of the cap is not written. The order of
//! use is kept in memory and starts from the files' modification times when the tier
//! opens.

use super::svccache::{Entry, Listed, Parts, Rows, Slice, listed_endpoint};
use oxrdf::Term;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The directory of the tier in a dataset's directory.
pub const DIR: &str = "service-cache";
/// The version of the files this build writes and reads ([`crate::formats::SERVICE_CACHE`]).
pub const VERSION: u32 = crate::formats::SERVICE_CACHE.writes;
const MAGIC: [u8; 8] = *b"SPKSVC\0\0";
/// The header: the magic, the version, the body's length and its digest.
const HEAD: usize = 8 + 4 + 8 + 32;
/// The fixed fields at the start of the body: the time, the key digest and the scope
/// digest, which opening the tier reads from every file.
const PREFIX: usize = 8 + 32 + 32;
const EXT: &str = ".svc";
const UNBOUND: u32 = u32::MAX;
/// How many times the largest file a decoded entry's row vectors may take. Each slot of
/// a row takes at least four bytes in the file and less than twenty times that in memory.
const ALLOC_FACTOR: u64 = 32;

type Digest32 = [u8; 32];

/// The number of the next temporary file, unique in the process, so that two tiers that
/// ever share a directory never write one temporary name.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// What a decoded term may keep allocated beyond its text: the strings of its parts and,
/// for a triple term, its box.
const TERM_EXTRA: u64 = 64;

/// The allowance a lookup admits.
type Admit<'a> = &'a dyn Fn(u64) -> crate::error::Result<()>;

fn digest(s: &[u8]) -> Digest32 {
    Sha256::digest(s).into()
}

fn hex(d: &Digest32) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in d {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(s: &str) -> Option<Digest32> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut d = [0u8; 32];
    for (i, o) in d.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(d)
}

/// What the tier knows of one file without reading it.
struct FileInfo {
    size: u64,
    /// the tick of the last use, for eviction
    used: u64,
    /// the id of the entry, which `se:cacheLs` shows
    id: u64,
    /// the digest of the entry's scope, or `None` for a file of another version
    scope: Option<Digest32>,
    /// seconds since the Unix epoch when the entry was stored
    stored: u64,
    /// the generation of the file, new on every write
    ver: u64,
}

#[derive(Default)]
struct Index {
    files: HashMap<Digest32, FileInfo>,
    total: u64,
    tick: u64,
    /// the last generation handed out
    vers: u64,
}

impl Index {
    fn insert(&mut self, d: Digest32, info: FileInfo) {
        self.total = self.total.saturating_add(info.size);
        if let Some(old) = self.files.insert(d, info) {
            self.total = self.total.saturating_sub(old.size);
        }
    }

    fn take(&mut self, d: &Digest32) -> Option<FileInfo> {
        let old = self.files.remove(d)?;
        self.total = self.total.saturating_sub(old.size);
        Some(old)
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn next_ver(&mut self) -> u64 {
        self.vers += 1;
        self.vers
    }
}

/// The entries of one dataset's SERVICE cache on disk.
pub struct DiskTier {
    dir: PathBuf,
    /// the bytes the files may take, which [`DiskTier::resize`] changes under the index
    /// lock
    cap: AtomicU64,
    index: Mutex<Index>,
    /// set under the index lock once the tier is turned off, after which it writes,
    /// reads and removes nothing, so a query that still holds it cannot touch a
    /// directory that a later tier uses
    retired: AtomicBool,
}

/// What a removal by scope did to the files: the key digests it removed and those of
/// files it could not remove, which stay, and whether the removals are durable.
pub(crate) struct Removal {
    removed: HashSet<Digest32>,
    stuck: HashSet<Digest32>,
    /// the failure of the directory sync that makes the removals durable
    pub(crate) synced: std::io::Result<()>,
}

impl Removal {
    fn none() -> Removal {
        Removal {
            removed: HashSet::new(),
            stuck: HashSet::new(),
            synced: Ok(()),
        }
    }

    /// How many files were removed.
    pub(crate) fn count(&self) -> u64 {
        self.removed.len() as u64
    }

    /// Whether the file of the request `key` was removed.
    pub(crate) fn removed(&self, key: &str) -> bool {
        self.removed.contains(&digest(key.as_bytes()))
    }

    /// Whether the file of the request `key` could not be removed and stays.
    pub(crate) fn stuck(&self, key: &str) -> bool {
        self.stuck.contains(&digest(key.as_bytes()))
    }
}

impl DiskTier {
    /// The tier in `dir`, of at most `cap` bytes, created when missing. Leftover
    /// temporary files are removed, and so are files over the cap, the least recently
    /// modified first.
    pub fn open(dir: PathBuf, cap: u64) -> std::io::Result<DiskTier> {
        std::fs::create_dir_all(&dir)?;
        let mut found = Vec::new();
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') && name.contains(".tmp-") {
                let _ = std::fs::remove_file(e.path());
                continue;
            }
            let Some(d) = name.strip_suffix(EXT).and_then(unhex) else {
                continue;
            };
            let meta = e.metadata()?;
            let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
            match read_prefix(&e.path()) {
                Ok(Some((scope, stored))) => {
                    found.push((mtime, d, meta.len(), Some(scope), stored));
                }
                // another version: kept, and counted
                Ok(None) => found.push((mtime, d, meta.len(), None, 0)),
                Err(_) => {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        found.sort_by_key(|f| f.0);
        let mut index = Index::default();
        for (_, d, size, scope, stored) in found {
            let used = index.next_tick();
            let ver = index.next_ver();
            index.insert(
                d,
                FileInfo {
                    size,
                    used,
                    id: super::svccache::next_id(),
                    scope,
                    stored,
                    ver,
                },
            );
        }
        let tier = DiskTier {
            dir,
            cap: AtomicU64::new(cap),
            index: Mutex::new(index),
            retired: AtomicBool::new(false),
        };
        tier.evict(&mut tier.lock(), None);
        Ok(tier)
    }

    /// The directory of the tier.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The bytes the files may take.
    pub fn capacity(&self) -> u64 {
        self.cap.load(Ordering::Acquire)
    }

    /// Change the cap of the tier in use, evicting the least recently used files over
    /// it, under the index lock, so that a write or removal running meanwhile sees the
    /// tier before the change or after it and never a second index of its directory.
    pub fn resize(&self, cap: u64) {
        let mut index = self.lock();
        self.cap.store(cap, Ordering::Release);
        self.evict(&mut index, None);
    }

    /// Stop using the tier: from now on it writes, reads and removes nothing. Its index
    /// is emptied under its lock, so a query that holds the tier and the store that
    /// removes or reuses its directory never act on the same files.
    pub fn retire(&self) {
        let mut index = self.lock();
        self.retired.store(true, Ordering::Release);
        *index = Index::default();
    }

    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// The bytes the files take.
    pub fn bytes(&self) -> u64 {
        self.lock().total
    }

    /// The number of files, of every version.
    pub fn entries(&self) -> usize {
        self.lock().files.len()
    }

    /// The largest file the tier writes.
    pub fn max_entry_bytes(&self) -> u64 {
        self.capacity() / 8
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path(&self, d: &Digest32) -> PathBuf {
        self.dir.join(format!("{}{EXT}", hex(d)))
    }

    /// Remove the file of `d` and forget it, under the index lock. A file that cannot
    /// be removed stays in the index, and the result says whether it is gone.
    fn drop_file(&self, index: &mut Index, d: &Digest32) -> bool {
        match std::fs::remove_file(self.path(d)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        index.take(d);
        true
    }

    /// Remove the file of `d` if it is still generation `ver`.
    fn drop_if(&self, d: &Digest32, ver: u64) {
        let mut index = self.lock();
        if index.files.get(d).is_some_and(|i| i.ver == ver) {
            self.drop_file(&mut index, d);
        }
    }

    /// Make the removals so far durable.
    fn sync(&self) -> std::io::Result<()> {
        crate::fsutil::sync_dir(&self.dir)
    }

    /// The live entry of the request `key`, read from its file. An entry older than
    /// `ttl` (when it is not zero) is removed. `admit` admits the bytes of the file
    /// before they are read, more of them if the file grew since it was indexed, and
    /// what decoding allocates before it allocates it, row by row. A refusal is the
    /// lookup's error.
    pub fn get(
        &self,
        key: &str,
        ttl: Duration,
        admit: Admit<'_>,
    ) -> crate::error::Result<Option<Entry>> {
        let d = digest(key.as_bytes());
        let (id, ver, size) = {
            let mut index = self.lock();
            if self.is_retired() {
                return Ok(None);
            }
            let tick = index.next_tick();
            let Some(info) = index.files.get_mut(&d) else {
                return Ok(None);
            };
            if info.scope.is_none() {
                return Ok(None);
            }
            info.used = tick;
            (info.id, info.ver, info.size)
        };
        admit(size)?;
        let bytes = match read_bounded(&self.path(&d), self.max_entry_bytes(), size, admit)? {
            Some(b) => b,
            None => {
                self.drop_if(&d, ver);
                return Ok(None);
            }
        };
        let e = match decode(&bytes, Check::Key(key), self.max_entry_bytes(), admit)? {
            Ok(e) => e,
            Err(Bad::Version) => return Ok(None),
            Err(Bad::Damaged) => {
                self.drop_if(&d, ver);
                return Ok(None);
            }
        };
        drop(bytes);
        if expired(e.stored, ttl) {
            self.drop_if(&d, ver);
            return Ok(None);
        }
        // a file removed or replaced while it was read answers nothing
        if self.lock().files.get(&d).is_none_or(|i| i.ver != ver) {
            return Ok(None);
        }
        let mut e = e.entry.expect("decoded with its key");
        e.id = id;
        Ok(Some(e))
    }

    /// Write the entry `e`, unless its file would be larger than an eighth of the cap or
    /// a file of another version holds its key. The entry keeps its id. A failed write
    /// leaves the tier as it was.
    pub fn put(&self, e: &Entry) {
        let d = digest(e.key().as_bytes());
        let Some(bytes) = encode(e, self.max_entry_bytes()) else {
            // the entry replaces the request's earlier one, which must not answer for it
            // from disk when the new one is kept in memory only
            let mut index = self.lock();
            if !self.is_retired() && index.files.get(&d).is_some_and(|i| i.scope.is_some()) {
                self.drop_file(&mut index, &d);
            }
            return;
        };
        let path = self.path(&d);
        let tmp = self.dir.join(format!(
            ".{}.tmp-{}-{}",
            hex(&d),
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if self.is_retired() || std::fs::write(&tmp, &bytes).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let mut index = self.lock();
        if self.is_retired()
            || index.files.get(&d).is_some_and(|i| i.scope.is_none())
            || std::fs::rename(&tmp, &path).is_err()
        {
            drop(index);
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let used = index.next_tick();
        let ver = index.next_ver();
        index.insert(
            d,
            FileInfo {
                size: bytes.len() as u64,
                used,
                id: e.id,
                scope: Some(digest(e.scope().as_bytes())),
                stored: secs(e.stored),
                ver,
            },
        );
        self.evict(&mut index, Some(d));
    }

    /// Remove the least recently used files until the files fit the cap, keeping `keep`
    /// unless it alone is over it. A file that cannot be removed is passed over.
    fn evict(&self, index: &mut Index, keep: Option<Digest32>) {
        let mut stuck = HashSet::new();
        let cap = self.capacity();
        while index.total > cap {
            let others = index
                .files
                .iter()
                .filter(|(d, _)| Some(**d) != keep && !stuck.contains(*d));
            let victim = match others.min_by_key(|(_, i)| i.used) {
                Some((d, _)) => *d,
                None => match keep {
                    Some(k) if !stuck.contains(&k) => k,
                    _ => return,
                },
            };
            if !self.drop_file(index, &victim) {
                stuck.insert(victim);
            }
        }
    }

    /// Drop the entry of the request `key`, with the directory synced after. A file
    /// of another version stays, since a newer release may read it, and only a clear
    /// and eviction remove it. The error is that of a file that stays or of the sync.
    pub fn remove(&self, key: &str) -> std::io::Result<()> {
        let d = digest(key.as_bytes());
        let mut index = self.lock();
        if self.is_retired() || !index.files.get(&d).is_some_and(|i| i.scope.is_some()) {
            return Ok(());
        }
        if !self.drop_file(&mut index, &d) {
            return Err(std::io::Error::other(
                "a SERVICE cache file cannot be removed",
            ));
        }
        drop(index);
        self.sync()
    }

    /// The live entries of `scope`, but for those whose keys are in `skip`. `admit`
    /// admits the listing's own storage before it grows and each file's bytes before
    /// it is read, and a refusal is the listing's error.
    pub fn list(
        &self,
        scope: &str,
        ttl: Duration,
        skip: &HashSet<Arc<str>>,
        admit: Admit<'_>,
    ) -> crate::error::Result<Vec<Listed>> {
        let scope = digest(scope.as_bytes());
        admit((skip.len() as u64).saturating_mul((size_of::<Digest32>() + 8) as u64))?;
        let skip: HashSet<Digest32> = skip.iter().map(|k| digest(k.as_bytes())).collect();
        type Candidate = (Digest32, u64, u64, u64);
        // an expired file is passed over without reading it
        let mine: Vec<Candidate> = {
            let index = self.lock();
            if self.is_retired() {
                return Ok(Vec::new());
            }
            let live = |(d, i): &(&Digest32, &FileInfo)| {
                let stored = UNIX_EPOCH.checked_add(Duration::from_secs(i.stored));
                i.scope == Some(scope)
                    && !skip.contains(*d)
                    && stored.is_some_and(|s| !expired(s, ttl))
            };
            let n = index.files.iter().filter(live).count();
            admit((n as u64).saturating_mul(size_of::<Candidate>() as u64))?;
            let mut mine = Vec::with_capacity(n);
            mine.extend(
                index
                    .files
                    .iter()
                    .filter(live)
                    .map(|(d, i)| (*d, i.id, i.ver, i.size)),
            );
            mine
        };
        let mut out = Vec::new();
        for (d, id, ver, size) in mine {
            admit(size)?;
            let bytes = match read_bounded(&self.path(&d), self.max_entry_bytes(), size, admit)? {
                Some(b) => b,
                None => continue,
            };
            let check = Check::Digests { key: d, scope };
            let e = match decode(&bytes, check, self.max_entry_bytes(), admit)? {
                Ok(e) => e,
                Err(Bad::Version) => continue,
                Err(Bad::Damaged) => {
                    self.drop_if(&d, ver);
                    continue;
                }
            };
            if expired(e.stored, ttl) || self.lock().files.get(&d).is_none_or(|i| i.ver != ver) {
                continue;
            }
            admit(size_of::<Listed>() as u64)?;
            out.push(Listed {
                id,
                endpoint: e.endpoint,
                pattern: e.pattern,
                binding: e.binding,
                rows: e.rows,
                bytes: bytes.len() as u64,
                age: SystemTime::now()
                    .duration_since(e.stored)
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Remove the entries of `scope`, or with `id` only the entry with that id, with
    /// one sync of the directory after them. Files of another version are not in any
    /// scope and stay.
    pub(crate) fn remove_in_scope(&self, scope: &str, id: Option<u64>) -> Removal {
        let scope = digest(scope.as_bytes());
        let mut index = self.lock();
        if self.is_retired() {
            return Removal::none();
        }
        let victims: Vec<Digest32> = index
            .files
            .iter()
            .filter(|(_, i)| i.scope == Some(scope) && id.is_none_or(|id| i.id == id))
            .map(|(d, _)| *d)
            .collect();
        let mut out = Removal::none();
        for d in victims {
            if self.drop_file(&mut index, &d) {
                out.removed.insert(d);
            } else {
                out.stuck.insert(d);
            }
        }
        drop(index);
        if !out.removed.is_empty() {
            out.synced = self.sync();
        }
        out
    }

    /// Remove every file, of every version, with the directory synced after. The error
    /// is that of the sync, or names how many files could not be removed and stay.
    pub fn clear(&self) -> std::io::Result<()> {
        let mut index = self.lock();
        if self.is_retired() {
            return Ok(());
        }
        let all: Vec<Digest32> = index.files.keys().copied().collect();
        let mut left = 0;
        for d in &all {
            if !self.drop_file(&mut index, d) {
                left += 1;
            }
        }
        drop(index);
        self.sync()?;
        match left {
            0 => Ok(()),
            n => Err(std::io::Error::other(format!(
                "{n} SERVICE cache files cannot be removed"
            ))),
        }
    }

    /// The seconds since the epoch when the entry of `key` was stored, if it is on disk.
    #[cfg(test)]
    fn stored(&self, key: &str) -> Option<u64> {
        self.lock()
            .files
            .get(&digest(key.as_bytes()))
            .map(|i| i.stored)
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn expired(stored: SystemTime, ttl: Duration) -> bool {
    !ttl.is_zero() && SystemTime::now().duration_since(stored).unwrap_or_default() >= ttl
}

/// The bytes of the file at `path`, or `None` when it cannot be read or is longer than
/// `limit`. `admitted` bytes were admitted for it. A file that grew since is admitted
/// for the difference with `admit` before it is read, and a refusal is the error.
fn read_bounded(
    path: &Path,
    limit: u64,
    admitted: u64,
    admit: Admit<'_>,
) -> crate::error::Result<Option<Vec<u8>>> {
    let Ok(f) = std::fs::File::open(path) else {
        return Ok(None);
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return Ok(None);
    };
    if len > limit.saturating_add(HEAD as u64) {
        return Ok(None);
    }
    let Ok(cap) = usize::try_from(len) else {
        return Ok(None);
    };
    if len > admitted {
        admit(len - admitted)?;
    }
    let mut out = Vec::with_capacity(cap);
    // at most the bytes admitted, should the file grow while it is read
    if f.take(len).read_to_end(&mut out).is_err() {
        return Ok(None);
    }
    Ok(Some(out))
}

/// The scope digest and the stored time of the file at `path`, `None` for a file of
/// another version, and an error for a file without the magic.
fn read_prefix(path: &Path) -> std::io::Result<Option<(Digest32, u64)>> {
    let mut f = std::fs::File::open(path)?;
    let mut b = [0u8; HEAD + PREFIX];
    let mut n = 0;
    while n < b.len() {
        match f.read(&mut b[n..])? {
            0 => break,
            k => n += k,
        }
    }
    if n < 12 || b[..8] != MAGIC {
        return Err(std::io::Error::other("not a SERVICE cache file"));
    }
    if u32::from_le_bytes(b[8..12].try_into().expect("4 bytes")) != VERSION {
        return Ok(None);
    }
    if n < b.len() {
        return Err(std::io::Error::other("a SERVICE cache file cut short"));
    }
    let body = &b[HEAD..];
    let stored = u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
    let scope: Digest32 = body[40..72].try_into().expect("32 bytes");
    Ok(Some((scope, stored)))
}

/// Why a file is not read.
#[derive(Debug, PartialEq, Eq)]
enum Bad {
    /// another version: kept for the release that wrote it
    Version,
    /// not a whole file of this version, or not the entry asked for: removed
    Damaged,
}

/// The file of `e`, or `None` when it would be longer than `limit` or a term has no
/// text.
fn encode(e: &Entry, limit: u64) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let put_str = |b: &mut Vec<u8>, s: &str| -> Option<()> {
        let n = u32::try_from(s.len()).ok().filter(|n| *n != UNBOUND)?;
        b.extend_from_slice(&n.to_le_bytes());
        b.extend_from_slice(s.as_bytes());
        Some(())
    };
    let over = |b: &Vec<u8>| b.len() as u64 > limit;
    body.extend_from_slice(&secs(e.stored).to_le_bytes());
    body.extend_from_slice(&digest(e.key().as_bytes()));
    body.extend_from_slice(&digest(e.scope().as_bytes()));
    let parts = e.parts();
    for n in [parts.scope(), parts.url(), parts.pattern(), e.slice.offset] {
        body.extend_from_slice(&(n as u64).to_le_bytes());
    }
    let limit = e.slice.limit.map_or(u64::MAX, |l| l as u64);
    body.extend_from_slice(&limit.to_le_bytes());
    put_str(&mut body, &listed_endpoint(e.endpoint()))?;
    put_str(&mut body, e.pattern())?;
    put_str(&mut body, e.binding())?;
    let rows = &e.rows;
    body.extend_from_slice(&u32::try_from(rows.vars.len()).ok()?.to_le_bytes());
    for v in &rows.vars {
        put_str(&mut body, v)?;
    }
    body.extend_from_slice(&(rows.rows.len() as u64).to_le_bytes());
    let mut text = String::new();
    for row in &rows.rows {
        if row.len() != rows.vars.len() {
            return None;
        }
        // a row without slots takes a byte, so that the count is bounded by the bytes
        if row.is_empty() {
            body.push(0);
        }
        for t in row {
            match t {
                None => body.extend_from_slice(&UNBOUND.to_le_bytes()),
                Some(t) => {
                    use std::fmt::Write;
                    text.clear();
                    let _ = write!(text, "{t}");
                    put_str(&mut body, &text)?;
                }
            }
        }
        if over(&body) {
            return None;
        }
    }
    if over(&body) {
        return None;
    }
    let mut out = Vec::with_capacity(HEAD + body.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(&digest(&body));
    out.extend_from_slice(&body);
    Some(out)
}

/// A decoded file: the entry when the key was given, or what a listing shows.
struct Decoded {
    entry: Option<Entry>,
    stored: SystemTime,
    /// for a listing only, empty for a lookup
    endpoint: String,
    pattern: String,
    binding: String,
    rows: usize,
}

/// A reader of a file's bytes that refuses to read past their end.
struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Bad> {
        if n > self.b.len() {
            return Err(Bad::Damaged);
        }
        let (head, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Bad> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, Bad> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Bad::Damaged)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, Bad> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Bad::Damaged)?,
        ))
    }

    fn usize(&mut self) -> Result<usize, Bad> {
        usize::try_from(self.u64()?).map_err(|_| Bad::Damaged)
    }

    fn digest(&mut self) -> Result<Digest32, Bad> {
        self.take(32)?.try_into().map_err(|_| Bad::Damaged)
    }

    /// A string of `n` bytes, checked against the bytes left before it is copied.
    fn text(&mut self, n: u32) -> Result<&'a str, Bad> {
        let n = usize::try_from(n).map_err(|_| Bad::Damaged)?;
        std::str::from_utf8(self.take(n)?).map_err(|_| Bad::Damaged)
    }

    /// A string, borrowed from the bytes.
    fn str(&mut self) -> Result<&'a str, Bad> {
        let n = self.u32()?;
        if n == UNBOUND {
            return Err(Bad::Damaged);
        }
        self.text(n)
    }

    #[cfg(test)]
    fn string(&mut self) -> Result<String, Bad> {
        Ok(self.str()?.to_string())
    }
}

/// Decode a file. With `key`, the file must be that key's, and the entry is built.
/// What a decoded file must match: the key of a lookup, which its key digest and scope
/// digest must be those of, or the digests a listing expects.
#[derive(Clone, Copy)]
enum Check<'a> {
    Key(&'a str),
    Digests { key: Digest32, scope: Digest32 },
}

/// Decode a file, admitting with `admit` what it allocates before it allocates it: the
/// strings a listing keeps, and the variable names, the rows' vectors and each row with
/// its terms of an entry. The outer error is a refusal of `admit`.
fn decode(
    bytes: &[u8],
    check: Check<'_>,
    limit: u64,
    admit: Admit<'_>,
) -> crate::error::Result<Result<Decoded, Bad>> {
    let mut r = Reader { b: bytes };
    macro_rules! take {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(b) => return Ok(Err(b)),
            }
        };
    }
    if take!(r.take(8)) != MAGIC {
        return Ok(Err(Bad::Damaged));
    }
    if take!(r.u32()) != VERSION {
        return Ok(Err(Bad::Version));
    }
    let len = take!(r.usize());
    let sum = take!(r.digest());
    if r.b.len() != len || digest(r.b) != sum {
        return Ok(Err(Bad::Damaged));
    }
    let stored = take!(
        UNIX_EPOCH
            .checked_add(Duration::from_secs(take!(r.u64())))
            .ok_or(Bad::Damaged)
    );
    let key_digest = take!(r.digest());
    let scope_digest = take!(r.digest());
    let (key, want) = match check {
        Check::Key(k) => (Some(k), (digest(k.as_bytes()), None)),
        Check::Digests { key, scope } => (None, (key, Some(scope))),
    };
    if key_digest != want.0 || want.1.is_some_and(|s| s != scope_digest) {
        return Ok(Err(Bad::Damaged));
    }
    let (scope_len, url_len, pattern_len) = (take!(r.usize()), take!(r.usize()), take!(r.usize()));
    let offset = take!(r.usize());
    let slice = Slice {
        offset,
        limit: match take!(r.u64()) {
            u64::MAX => None,
            l => Some(take!(usize::try_from(l).map_err(|_| Bad::Damaged))),
        },
    };
    let endpoint = take!(r.str());
    let pattern = take!(r.str());
    let binding = take!(r.str());
    let width = take!(usize::try_from(take!(r.u32())).map_err(|_| Bad::Damaged));
    // every name takes at least its length
    if width > r.b.len() / 4 {
        return Ok(Err(Bad::Damaged));
    }
    let mut names = Vec::new();
    if key.is_some() {
        admit((width as u64).saturating_mul(size_of::<String>() as u64))?;
        names.reserve_exact(width);
    }
    for _ in 0..width {
        let v = take!(r.str());
        if key.is_some() {
            admit(v.len() as u64)?;
            names.push(v.to_string());
        }
    }
    let count = take!(r.usize());
    // every slot of a row takes at least its length and a row without slots a byte, so
    // the bytes left bound the count, and the rows' vectors are bounded before they are
    // allocated
    let per_row = take!(width.checked_mul(4).ok_or(Bad::Damaged)).max(1);
    let slots = take!(
        width
            .checked_mul(size_of::<Option<Term>>())
            .ok_or(Bad::Damaged)
    );
    let row_alloc = take!(
        slots
            .checked_add(size_of::<Vec<Option<Term>>>())
            .ok_or(Bad::Damaged)
    );
    let alloc = take!(count.checked_mul(row_alloc).ok_or(Bad::Damaged));
    if count > r.b.len() / per_row || alloc as u64 > limit.saturating_mul(ALLOC_FACTOR) {
        return Ok(Err(Bad::Damaged));
    }
    let Some(key) = key else {
        admit(
            (endpoint.len() as u64)
                .saturating_add(pattern.len() as u64)
                .saturating_add(binding.len() as u64),
        )?;
        return Ok(Ok(Decoded {
            entry: None,
            stored,
            endpoint: endpoint.to_string(),
            pattern: pattern.to_string(),
            binding: binding.to_string(),
            rows: count,
        }));
    };
    admit((count as u64).saturating_mul(size_of::<Vec<Option<Term>>>() as u64))?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        // the row's terms are weighed from their lengths before any is parsed
        let mut peek = Reader { b: r.b };
        let mut terms = 0u64;
        for _ in 0..width {
            let n = take!(peek.u32());
            if n != UNBOUND {
                take!(peek.take(take!(usize::try_from(n).map_err(|_| Bad::Damaged))));
                terms = terms.saturating_add(u64::from(n) + TERM_EXTRA);
            }
        }
        admit((slots as u64).saturating_add(terms))?;
        let mut row = Vec::with_capacity(width);
        if width == 0 && take!(r.u8()) != 0 {
            return Ok(Err(Bad::Damaged));
        }
        for _ in 0..width {
            let n = take!(r.u32());
            row.push(if n == UNBOUND {
                None
            } else {
                Some(take!(
                    take!(r.text(n)).parse::<Term>().map_err(|_| Bad::Damaged)
                ))
            });
        }
        rows.push(row);
    }
    if !r.b.is_empty() {
        return Ok(Err(Bad::Damaged));
    }
    let parts = Parts::from_lens(scope_len, url_len, pattern_len);
    let total = take!(
        scope_len
            .checked_add(url_len)
            .and_then(|n| n.checked_add(pattern_len))
            .and_then(|n| n.checked_add(3))
            .ok_or(Bad::Damaged)
    );
    if total > key.len() {
        return Ok(Err(Bad::Damaged));
    }
    admit((key.len() + size_of::<Entry>() + size_of::<Rows>()) as u64)?;
    let entry = Entry::new(
        Arc::from(key),
        parts,
        slice,
        Arc::new(Rows { vars: names, rows }),
        stored,
    );
    // the scope digest is that of the key's own scope
    if digest(entry.scope().as_bytes()) != scope_digest {
        return Ok(Err(Bad::Damaged));
    }
    Ok(Ok(Decoded {
        entry: Some(entry),
        stored,
        endpoint: String::new(),
        pattern: String::new(),
        binding: String::new(),
        rows: count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::svccache::key;
    use oxrdf::{Literal, NamedNode, Triple};

    fn rows() -> Arc<Rows> {
        let n = |s: &str| NamedNode::new(s).unwrap();
        Arc::new(Rows {
            vars: vec!["a".into(), "b".into()],
            rows: vec![
                vec![
                    Some(n("http://e/x").into()),
                    Some(
                        Literal::new_language_tagged_literal("chat \"é\"\n", "fr")
                            .unwrap()
                            .into(),
                    ),
                ],
                vec![
                    None,
                    Some(Term::Triple(Box::new(Triple::new(
                        n("urn:s"),
                        n("urn:p"),
                        Literal::from(3),
                    )))),
                ],
                vec![Some(Literal::from(1.5).into()), None],
            ],
        })
    }

    fn entry(scope: &str, pattern: &str, at: SystemTime) -> Entry {
        let url = "http://user:pw@e/sparql";
        let v: Term = Literal::from(7).into();
        let k = key(scope, url, pattern, &[("x", &v)]);
        Entry::new(
            Arc::from(k.as_str()),
            Parts::of(scope, url, pattern),
            Slice {
                offset: 2,
                limit: Some(10),
            },
            rows(),
            at,
        )
    }

    #[test]
    fn an_entry_is_read_back_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let t = DiskTier::open(dir.path().join(DIR), 1 << 20).unwrap();
        let e = entry("alice", "?s ?p ?o", SystemTime::now());
        t.put(&e);
        assert_eq!(t.entries(), 1);
        let back = t
            .get(e.key(), Duration::from_secs(60), &|_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.key(), e.key());
        assert_eq!(back.scope(), "alice");
        assert_eq!(back.endpoint(), "http://user:pw@e/sparql");
        assert_eq!(back.pattern(), "?s ?p ?o");
        assert_eq!(
            back.slice,
            Slice {
                offset: 2,
                limit: Some(10)
            }
        );
        assert_eq!(back.rows.vars, e.rows.vars);
        assert_eq!(back.rows.rows, e.rows.rows);
        assert_eq!(t.stored(e.key()), Some(secs(e.stored)));
        // the key and the credentials are not written
        let raw = std::fs::read_dir(dir.path().join(DIR))
            .unwrap()
            .map(|f| std::fs::read(f.unwrap().path()).unwrap())
            .next()
            .unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("alice"), "the scope is written");
        assert!(!text.contains("pw@"), "the password is written");
        // another key with the same digest prefix is not this one
        assert!(
            t.get("another key", Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn entries_outlive_the_tier_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let old = entry("a", "old", SystemTime::now() - Duration::from_secs(7200));
        let new = entry("a", "new", SystemTime::now());
        {
            let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
            t.put(&old);
            t.put(&new);
        }
        let t = DiskTier::open(path, 1 << 20).unwrap();
        assert_eq!(t.entries(), 2);
        assert_eq!(
            t.list("a", Duration::from_secs(3600), &HashSet::new(), &|_| Ok(()))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            t.list("b", Duration::ZERO, &HashSet::new(), &|_| Ok(()))
                .unwrap()
                .len(),
            0
        );
        assert!(
            t.get(new.key(), Duration::from_secs(3600), &|_| Ok(()))
                .unwrap()
                .is_some()
        );
        assert!(
            t.get(old.key(), Duration::from_secs(3600), &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(t.entries(), 1, "the expired file is removed");
        assert!(
            t.get(old.key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_files_stay_under_the_cap_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let one = encode(&entry("a", "p0", SystemTime::now()), u64::MAX)
            .unwrap()
            .len() as u64;
        // room for three files, and an eighth of the cap holds one
        let cap = (one * 3).max(one * 8 + 64);
        let t = DiskTier::open(dir.path().join(DIR), cap).unwrap();
        let es: Vec<Entry> = (0..12)
            .map(|i| entry("a", &format!("p{i}"), SystemTime::now()))
            .collect();
        for (i, e) in es.iter().enumerate() {
            t.put(e);
            // keep the first one in use
            if i > 0 {
                assert!(
                    t.get(es[0].key(), Duration::ZERO, &|_| Ok(()))
                        .unwrap()
                        .is_some(),
                    "{i}"
                );
            }
            assert!(t.bytes() <= cap, "{} > {cap}", t.bytes());
        }
        let files = std::fs::read_dir(dir.path().join(DIR)).unwrap().count();
        assert_eq!(files, t.entries());
        assert!(t.entries() < es.len(), "nothing was evicted");
        assert!(
            t.get(es[0].key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_some()
        );
        assert!(
            t.get(es[1].key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        // an entry over an eighth of the cap is not written
        let small = DiskTier::open(dir.path().join("small"), one * 4).unwrap();
        small.put(&es[0]);
        assert_eq!(small.entries(), 0);
    }

    /// A file cut short, changed or of another key is a miss and is removed. A file of
    /// another version is a miss and is kept.
    #[test]
    fn damaged_and_unknown_files_are_misses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        let e = entry("a", "p", SystemTime::now());
        t.put(&e);
        let file = t.path(&digest(e.key().as_bytes()));
        let good = std::fs::read(&file).unwrap();
        // every way of cutting the file short
        for n in [0, 4, 11, HEAD, HEAD + 10, good.len() - 1] {
            std::fs::write(&file, &good[..n]).unwrap();
            t.lock().insert(
                digest(e.key().as_bytes()),
                FileInfo {
                    size: n as u64,
                    used: 0,
                    id: 1,
                    scope: Some([0; 32]),
                    stored: 0,
                    ver: 0,
                },
            );
            assert!(
                t.get(e.key(), Duration::ZERO, &|_| Ok(()))
                    .unwrap()
                    .is_none(),
                "{n}"
            );
            assert!(!file.exists(), "{n}: kept");
        }
        // a flipped byte in the body
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(
            decode(&bad, Check::Key(e.key()), u64::MAX, &|_| Ok(()))
                .unwrap()
                .err(),
            Some(Bad::Damaged)
        );
        // another key's file under this name
        assert_eq!(
            decode(&good, Check::Key("other"), u64::MAX, &|_| Ok(()))
                .unwrap()
                .err(),
            Some(Bad::Damaged)
        );
        // another version
        let mut newer = good.clone();
        newer[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
        std::fs::write(&file, &newer).unwrap();
        drop(t);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        assert_eq!(t.entries(), 1, "the newer file is counted");
        assert!(
            t.get(e.key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        assert!(file.exists(), "the newer file is kept");
        assert_eq!(
            decode(&newer, Check::Key(e.key()), u64::MAX, &|_| Ok(()))
                .unwrap()
                .err(),
            Some(Bad::Version)
        );
        t.clear().unwrap();
        assert!(!file.exists(), "clear removes every version");
        // a file without the magic and a leftover temporary file go at open
        std::fs::write(&file, b"not a cache file").unwrap();
        std::fs::write(path.join(".x.tmp-1-1"), b"partial").unwrap();
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        assert_eq!(t.entries(), 0);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    }

    /// Counts and lengths that the bytes cannot hold are refused before anything is
    /// allocated for them.
    #[test]
    fn counts_past_the_end_are_refused() {
        let e = entry("a", "p", SystemTime::now());
        let good = encode(&e, u64::MAX).unwrap();
        let reseal = |mut body: Vec<u8>| {
            let mut out = good[..8 + 4].to_vec();
            out.extend_from_slice(&(body.len() as u64).to_le_bytes());
            out.extend_from_slice(&digest(&body));
            out.append(&mut body);
            out
        };
        let body = good[HEAD..].to_vec();
        // the row count is the u64 before the rows: the body of the vars ends there
        let rows_at = {
            let mut r = Reader { b: &body };
            r.take(PREFIX + 8 * 5).unwrap();
            for _ in 0..3 {
                r.string().unwrap();
            }
            let w = r.u32().unwrap();
            for _ in 0..w {
                r.string().unwrap();
            }
            body.len() - r.b.len()
        };
        let mut b = body.clone();
        b[rows_at..rows_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            decode(&reseal(b), Check::Key(e.key()), u64::MAX, &|_| Ok(()))
                .unwrap()
                .err(),
            Some(Bad::Damaged)
        );
        let mut b = body.clone();
        // the endpoint's length
        let at = PREFIX + 8 * 5;
        b[at..at + 4].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        assert_eq!(
            decode(&reseal(b), Check::Key(e.key()), u64::MAX, &|_| Ok(()))
                .unwrap()
                .err(),
            Some(Bad::Damaged)
        );
        assert!(
            decode(&reseal(body), Check::Key(e.key()), u64::MAX, &|_| Ok(()))
                .unwrap()
                .is_ok()
        );
    }

    #[test]
    fn removal_by_scope_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let t = DiskTier::open(dir.path().join(DIR), 1 << 20).unwrap();
        let (a, b, c) = (
            entry("a", "1", SystemTime::now()),
            entry("a", "2", SystemTime::now()),
            entry("ab", "1", SystemTime::now()),
        );
        for e in [&a, &b, &c] {
            t.put(e);
        }
        assert_eq!(t.remove_in_scope("a", Some(c.id)).count(), 0);
        assert_eq!(t.remove_in_scope("a", Some(a.id)).count(), 1);
        assert_eq!(t.remove_in_scope("a", None).count(), 1);
        assert_eq!(t.entries(), 1);
        let l = t
            .list("ab", Duration::ZERO, &HashSet::new(), &|_| Ok(()))
            .unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].endpoint, "http://e/sparql");
        assert_eq!(l[0].id, c.id);
    }

    /// A file of another version is not overwritten by an entry of its key, a file
    /// under the name of another key or with another scope is not listed and is removed,
    /// and a file that cannot be removed stays and is not counted as removed.
    #[test]
    fn files_of_other_versions_keys_and_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        let (a, b) = (
            entry("a", "1", SystemTime::now()),
            entry("a", "2", SystemTime::now()),
        );
        t.put(&a);
        t.put(&b);
        let fa = t.path(&digest(a.key().as_bytes()));
        let fb = t.path(&digest(b.key().as_bytes()));
        // b's file under a's name: listed under neither, and removed
        std::fs::copy(&fb, &fa).unwrap();
        let l = t
            .list("a", Duration::ZERO, &HashSet::new(), &|_| Ok(()))
            .unwrap();
        assert_eq!(l.iter().map(|l| l.id).collect::<Vec<_>>(), vec![b.id]);
        assert!(!fa.exists());
        assert!(
            t.get(a.key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        // a newer file of b's key stays when b is written again
        let mut newer = std::fs::read(&fb).unwrap();
        newer[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
        std::fs::write(&fb, &newer).unwrap();
        drop(t);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        t.put(&b);
        assert_eq!(std::fs::read(&fb).unwrap(), newer, "the newer file is kept");
        assert!(
            t.get(b.key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        // a file that cannot be removed is not reported as removed
        t.put(&a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).unwrap();
            let locked = std::fs::remove_file(&fa).is_err();
            if locked {
                assert_eq!(t.remove_in_scope("a", Some(a.id)).count(), 0);
                assert!(
                    t.get(a.key(), Duration::ZERO, &|_| Ok(()))
                        .unwrap()
                        .is_some(),
                    "still there"
                );
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            if !locked {
                // the file went in the probe (run as root): write it again
                t.remove(a.key()).unwrap();
                t.put(&a);
            }
        }
        assert_eq!(t.remove_in_scope("a", Some(a.id)).count(), 1);
        assert!(!fa.exists());
    }

    /// A read and a listing admit the file's bytes before they read it, and what they
    /// decode before they decode it, and stop at a refusal.
    #[test]
    fn reads_and_listings_admit_before_they_allocate() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let t = DiskTier::open(dir.path().join(DIR), 1 << 20).unwrap();
        let e = entry("a", "p", SystemTime::now());
        t.put(&e);
        let size = t.bytes();
        let asked = Cell::new(Vec::new());
        let note = |n: u64| {
            let mut v = asked.take();
            v.push(n);
            asked.set(v);
            Ok(())
        };
        assert!(t.get(e.key(), Duration::ZERO, &note).unwrap().is_some());
        let v = asked.take();
        assert_eq!(v[0], size, "{v:?}");
        assert!(v[1..].iter().sum::<u64>() >= e.rows.bytes(), "{v:?}");
        let refuse = |_| Err(crate::error::Error::invalid("over budget"));
        assert!(t.get(e.key(), Duration::ZERO, &refuse).is_err());
        assert!(
            t.list("a", Duration::ZERO, &HashSet::new(), &refuse)
                .is_err()
        );
        // the file and no more refuses the rows
        let left = Cell::new(size);
        let file_only = |n: u64| match left.get().checked_sub(n) {
            Some(l) => {
                left.set(l);
                Ok(())
            }
            None => Err(crate::error::Error::invalid("over budget")),
        };
        assert!(t.get(e.key(), Duration::ZERO, &file_only).is_err());
        assert_eq!(
            t.list("a", Duration::ZERO, &HashSet::new(), &note)
                .unwrap()
                .len(),
            1
        );
        assert!(asked.take().contains(&size));
        assert_eq!(t.entries(), 1, "a refusal removes nothing");
    }

    /// A keyed removal leaves a file of another version, and an entry that replaces
    /// a request's file but is too large to write takes the old file away with it.
    #[test]
    fn keyed_removal_keeps_other_versions_and_replacement_drops_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        let e = entry("a", "p", SystemTime::now());
        t.put(&e);
        let file = t.path(&digest(e.key().as_bytes()));
        let mut newer = std::fs::read(&file).unwrap();
        newer[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
        std::fs::write(&file, &newer).unwrap();
        drop(t);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        t.remove(e.key()).unwrap();
        assert_eq!(t.remove_in_scope("a", None).count(), 0);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            newer,
            "the newer file is kept"
        );
        t.clear().unwrap();
        // a stale file of this version goes when its replacement cannot be written
        t.put(&e);
        assert!(file.exists());
        let big = Entry::new(
            Arc::from(e.key()),
            e.parts(),
            Slice::ALL,
            Arc::new(Rows {
                vars: vec!["a".into()],
                rows: (0..40_000)
                    .map(|i| vec![Some(Literal::from(i).into())])
                    .collect(),
            }),
            SystemTime::now(),
        );
        t.put(&big);
        assert!(!file.exists(), "the stale file answers for the new entry");
        assert_eq!(t.entries(), 0);
    }

    /// A retired tier writes, reads and removes nothing, and a resize evicts under the
    /// lock of the tier in use.
    #[test]
    fn retired_and_resized_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let t = DiskTier::open(path.clone(), 1 << 20).unwrap();
        let es: Vec<Entry> = (0..4)
            .map(|i| entry("a", &format!("p{i}"), SystemTime::now()))
            .collect();
        for e in &es {
            t.put(e);
        }
        let one = t.bytes() / 4;
        t.resize(one * 2);
        assert_eq!(t.entries(), 2);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
        t.retire();
        assert!(
            t.get(es[3].key(), Duration::ZERO, &|_| Ok(()))
                .unwrap()
                .is_none()
        );
        t.put(&es[0]);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2, "written");
        assert_eq!(t.remove_in_scope("a", None).count(), 0);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2, "removed");
    }
}
