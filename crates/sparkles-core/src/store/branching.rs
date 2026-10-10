//! The branches of a persistent dataset: the branch table (`branches.json`), branch
//! stores opened lazily, creation, deletion, protection, the holds that keep upstream
//! generations on disk, merge records and merge bases.
//!
//! The dataset's own store is the branch `main` and owns the [`BranchSet`]. Every other
//! branch is a [`Store`] rooted at `<dataset>/branches/<branch id>/`, which the set opens
//! on first use and keeps open. Branch stores refer to the set weakly, so that dropping
//! the dataset's store closes them all.
//!
//! Lock order: a store's writer lock, then the set's table lock, then a store's history
//! lock. An upstream's history lock may be taken while a branch's writer lock is held,
//! never the reverse.

use super::link::{LINK_FILE, LinkFile, Segment};
use super::*;
use crate::branch::{
    self, BranchError, BranchErrorKind, BranchInfo, BranchOptions, BranchStorage, CommitRef, MAIN,
    NamedCommitRef,
};
use crate::history::{At, Hold};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap as Map, HashMap, HashSet};
use std::sync::Weak;

/// The branch table of a dataset.
pub const BRANCHES_FILE: &str = "branches.json";
/// The directory of the branch stores.
pub const BRANCHES_DIR: &str = "branches";
/// A branch store's identity file.
pub const BRANCH_FILE: &str = "branch.json";
/// A store's merge records.
pub const MERGES_FILE: &str = "merges.bin";

/// Configuration files a new branch copies from its upstream.
const COPIED_FILES: [&str; 14] = [
    "reasoning.json",
    "rdfs.json",
    "rdfs-schema.nt",
    crate::guard::config::CONFIG_FILE,
    crate::guard::config::SHACL_SHAPES_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXC_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXJ_FILE,
    "text.json",
    crate::geo::CONFIG_FILE,
    crate::vector::config::CONFIG_FILE,
    compaction::COMPACTION_FILE,
    describe::DESCRIBE_FILE,
    changelog::CHANGE_LOG_FILE,
    "origin.json",
];

// ------------------------------------------------------------------ files ------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FromRef {
    pub branch_id: uuid::Uuid,
    pub seq: u64,
}

impl From<FromRef> for CommitRef {
    fn from(f: FromRef) -> CommitRef {
        CommitRef {
            branch_id: f.branch_id,
            seq: f.seq,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenHold {
    pub branch_id: uuid::Uuid,
    pub generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    pub name: String,
    pub id: uuid::Uuid,
    pub ordinal: u16,
    pub from: FromRef,
    pub created: String,
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub note: Option<String>,
    /// the upstream generations the branch's link reads (empty once it rebuilt)
    #[serde(default)]
    pub holds: Vec<GenHold>,
    /// the upstream commit kept readable for merges
    #[serde(default)]
    pub base_hold: Option<FromRef>,
    /// set on a scratch branch (C17 §5.7)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch: Option<branch::Scratch>,
}

/// A deleted branch: kept so that the commits other branches merged from it still have
/// a known starting point.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Tombstone {
    pub name: String,
    pub id: uuid::Uuid,
    pub ordinal: u16,
    pub from: FromRef,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MainEntry {
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TableFile {
    /// 1, or 2 while the table lists retired branches, which an older build that reads
    /// format 1 would remove
    pub format: u32,
    pub dataset_id: uuid::Uuid,
    pub next_ordinal: u32,
    #[serde(default)]
    pub main: MainEntry,
    #[serde(default)]
    pub branches: Vec<Entry>,
    /// Deleted branches whose storage other branches still need: their starting points
    /// and log segments lie in the history of branches created from them. A retired
    /// branch has no name that requests reach, holds what it held, and goes once no
    /// branch, listed or retired, starts from it or reads its files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<Entry>,
    /// predicates whose cells never conflict in this dataset's merges (IRIs)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Tombstone>,
}

impl TableFile {
    fn new(dataset_id: uuid::Uuid) -> TableFile {
        TableFile {
            format: 1,
            dataset_id,
            next_ordinal: 1,
            main: MainEntry::default(),
            branches: Vec::new(),
            retired: Vec::new(),
            exempt: Vec::new(),
            deleted: Vec::new(),
        }
    }

    fn by_name(&self, name: &str) -> Option<&Entry> {
        self.branches.iter().find(|e| e.name == name)
    }

    fn by_id(&self, id: uuid::Uuid) -> Option<&Entry> {
        self.branches.iter().find(|e| e.id == id)
    }

    /// A listed or a retired branch.
    fn any_by_id(&self, id: uuid::Uuid) -> Option<&Entry> {
        self.by_id(id)
            .or_else(|| self.retired.iter().find(|e| e.id == id))
    }

    /// The listed and the retired branches.
    fn all(&self) -> impl Iterator<Item = &Entry> {
        self.branches.iter().chain(self.retired.iter())
    }

    /// Whether a listed or retired branch starts from branch `id` or reads its files.
    fn has_dependents(&self, id: uuid::Uuid) -> bool {
        self.all().any(|c| {
            c.id != id && (c.from.branch_id == id || c.holds.iter().any(|h| h.branch_id == id))
        })
    }

    /// Retired branches that nothing depends on any more, moved to the tombstones:
    /// their ids, whose directories can go.
    fn prune_retired(&mut self) -> Vec<uuid::Uuid> {
        let mut gone = Vec::new();
        while let Some(i) = self.retired.iter().position(|r| !self.has_dependents(r.id)) {
            let r = self.retired.remove(i);
            self.deleted.push(Tombstone {
                name: r.name.clone(),
                id: r.id,
                ordinal: r.ordinal,
                from: r.from,
            });
            gone.push(r.id);
        }
        gone
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BranchFile {
    pub format: u32,
    pub dataset_id: uuid::Uuid,
    pub name: String,
    pub id: uuid::Uuid,
    pub ordinal: u16,
    pub from: FromRef,
    pub created: String,
}

pub(crate) fn read_branch_file(root: &Path) -> Result<Option<BranchFile>> {
    let path = root.join(BRANCH_FILE);
    match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn read_table(root: &Path) -> Result<Option<TableFile>> {
    let path = root.join(BRANCHES_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let t: TableFile = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
    if t.format != 1 && t.format != 2 {
        return Err(Error::Corrupt(format!(
            "{} has format {}, this build reads 1 and 2",
            path.display(),
            t.format
        )));
    }
    Ok(Some(t))
}

fn write_table(root: &Path, t: &TableFile) -> Result<()> {
    commit::require_branch_reader(root)?;
    let mut t = t.clone();
    t.format = if t.retired.is_empty() { 1 } else { 2 };
    write_atomic(
        &root.join(BRANCHES_FILE),
        &serde_json::to_vec_pretty(&t).expect("the table serializes"),
    )
}

/// Write the name in branch `root`'s identity file, if it differs (after a rename).
fn sync_branch_file(root: &Path, name: &str) -> Result<()> {
    let Some(mut b) = read_branch_file(root)? else {
        return Ok(());
    };
    if b.name != name {
        b.name = name.to_string();
        write_atomic(
            &root.join(BRANCH_FILE),
            &serde_json::to_vec_pretty(&b).expect("serializes"),
        )?;
    }
    Ok(())
}

/// Write the branch table of a new dataset (a clone) whose data may hold blank nodes of
/// branch ordinals below `next_ordinal`.
pub(crate) fn write_initial_table(root: &Path, id: uuid::Uuid, next_ordinal: u64) -> Result<()> {
    let mut t = TableFile::new(id);
    t.next_ordinal = next_ordinal.min(u16::MAX as u64 + 1) as u32;
    write_table(root, &t)
}

/// The branch table of a database directory, read without opening it (`None` when the
/// dataset has no branches).
pub fn read_branch_table(root: &Path) -> Result<Option<serde_json::Value>> {
    Ok(read_table(root)?.map(|t| serde_json::to_value(t).expect("the table serializes")))
}

// ---------------------------------------------------------------- merge log ------

/// One merge record: the merge commit on this store and the source commit it merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MergeRec {
    pub seq: u64,
    pub source: CommitRef,
    pub resolved: u64,
    pub flags: u32,
}

/// A merge that was a fast-forward.
pub(crate) const MERGE_FAST_FORWARD: u32 = 1;
/// A commit that replays the source commit, in a replayed fast-forward.
pub(crate) const MERGE_REPLAYED: u32 = 2;

const MERGE_REC: usize = 48;

impl MergeRec {
    fn encode(&self) -> [u8; MERGE_REC] {
        let mut r = [0u8; MERGE_REC];
        r[0..8].copy_from_slice(&self.seq.to_le_bytes());
        r[8..24].copy_from_slice(self.source.branch_id.as_bytes());
        r[24..32].copy_from_slice(&self.source.seq.to_le_bytes());
        r[32..40].copy_from_slice(&self.resolved.to_le_bytes());
        r[40..44].copy_from_slice(&self.flags.to_le_bytes());
        let mut c = flate2::Crc::new();
        c.update(&r[..44]);
        r[44..48].copy_from_slice(&c.sum().to_le_bytes());
        r
    }

    fn decode(r: &[u8]) -> Option<MergeRec> {
        let mut c = flate2::Crc::new();
        c.update(&r[..44]);
        if c.sum().to_le_bytes() != r[44..48] {
            return None;
        }
        Some(MergeRec {
            seq: u64::from_le_bytes(r[0..8].try_into().ok()?),
            source: CommitRef {
                branch_id: uuid::Uuid::from_slice(&r[8..24]).ok()?,
                seq: u64::from_le_bytes(r[24..32].try_into().ok()?),
            },
            resolved: u64::from_le_bytes(r[32..40].try_into().ok()?),
            flags: u32::from_le_bytes(r[40..44].try_into().ok()?),
        })
    }
}

/// A store's merge records (`merges.bin`), in commit order.
#[derive(Default)]
pub(crate) struct MergeLog {
    path: Option<PathBuf>,
    pub recs: Vec<MergeRec>,
}

impl MergeLog {
    /// Read `merges.bin` of the store rooted at `root`, keeping the records of commits
    /// up to `head`: a record above it belongs to a merge whose commit never became
    /// durable, and is cut off the file.
    pub fn open(root: &Path, head: u64) -> Result<MergeLog> {
        let path = root.join(MERGES_FILE);
        let buf = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MergeLog {
                    path: Some(path),
                    recs: Vec::new(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        let mut recs = Vec::new();
        for r in buf.as_chunks::<MERGE_REC>().0 {
            match MergeRec::decode(r) {
                Some(m)
                    if m.seq <= head && recs.last().is_none_or(|l: &MergeRec| l.seq < m.seq) =>
                {
                    recs.push(m)
                }
                _ => break,
            }
        }
        let good = (recs.len() * MERGE_REC) as u64;
        if good != buf.len() as u64 {
            OpenOptions::new().write(true).open(&path)?.set_len(good)?;
        }
        Ok(MergeLog {
            path: Some(path),
            recs,
        })
    }

    /// Append a record durably (before the merge commit is written).
    pub fn append(&mut self, m: MergeRec) -> Result<()> {
        let Some(path) = &self.path else {
            self.recs.push(m);
            return Ok(());
        };
        let new = !path.exists();
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(&m.encode())?;
        f.sync_data()?;
        if new {
            sync_dir(path.parent().unwrap_or(Path::new(".")))?;
        }
        self.recs.push(m);
        Ok(())
    }

    /// Take back the last record, for commit `seq` that failed.
    pub fn undo(&mut self, seq: u64) -> Result<()> {
        if self.recs.last().is_some_and(|m| m.seq == seq) {
            self.recs.pop();
            if let Some(path) = &self.path {
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len((self.recs.len() * MERGE_REC) as u64)?;
            }
        }
        Ok(())
    }

    pub fn get(&self, seq: u64) -> Option<MergeRec> {
        self.recs
            .binary_search_by_key(&seq, |m| m.seq)
            .ok()
            .map(|i| self.recs[i])
    }
}

// ----------------------------------------------------------- store identity ------

/// The identity of a branch store (not `main`).
#[derive(Clone, Debug)]
pub(crate) struct Ident {
    pub name: String,
    pub ordinal: u16,
    /// the dataset's id (the store's own id is the branch id)
    pub dataset_id: uuid::Uuid,
    pub from: CommitRef,
}

/// How a store reaches the dataset's branch set.
#[derive(Default)]
pub(crate) enum SetRef {
    /// an in-memory store, or one opened outside a dataset
    #[default]
    None,
    /// the dataset's own store owns the set
    Owner(Arc<BranchSet>),
    /// a branch store
    Member(Weak<BranchSet>),
}

/// What a store knows about branches.
#[derive(Default)]
pub(crate) struct Branching {
    pub ident: Option<Ident>,
    /// the branch's name, which a rename changes (empty for `main`)
    pub name: parking_lot::RwLock<String>,
    /// the branch was deleted and is kept only for the branches created from it
    pub retired: AtomicBool,
    pub set: SetRef,
    /// writes other than merges are refused
    pub protected: AtomicBool,
    pub merges: Mutex<MergeLog>,
    /// the dataset's next branch ordinal: blank nodes of lower ordinals may exist
    pub next_ordinal: AtomicU64,
}

impl Branching {
    pub fn set(&self) -> Option<Arc<BranchSet>> {
        match &self.set {
            SetRef::None => None,
            SetRef::Owner(s) => Some(s.clone()),
            SetRef::Member(w) => w.upgrade(),
        }
    }
}

/// What a branch store is opened with.
pub(crate) struct OpenCtx {
    pub ident: Ident,
    pub set: Weak<BranchSet>,
    pub cache: Arc<BlockCache>,
    pub quota: Arc<quota::Quota>,
    pub protected: bool,
    pub next_ordinal: u64,
    /// generations whose block-cache identities the store's linked generation shares
    pub share: Vec<Arc<Generation>>,
}

// --------------------------------------------------------------- branch set ------

type MemoryBases = HashMap<(uuid::Uuid, u64), (CommitInfo, Arc<Snapshot>)>;

/// The branches of one persistent or in-memory dataset.
pub struct BranchSet {
    memory: bool,
    memory_bases: Mutex<MemoryBases>,
    root: PathBuf,
    dataset_id: uuid::Uuid,
    opts: StoreOptions,
    cache: Arc<BlockCache>,
    quota: Arc<quota::Quota>,
    table: Mutex<TableFile>,
    /// open branch stores (not `main`)
    stores: Mutex<HashMap<uuid::Uuid, Arc<Store>>>,
    /// serializes the opening of branch stores
    opening: Mutex<()>,
    /// merge records by the id of the branch that made the merge commit
    merges: parking_lot::RwLock<HashMap<uuid::Uuid, Vec<MergeRec>>>,
    /// listed branches whose directory is missing or unreadable
    broken: Mutex<HashSet<uuid::Uuid>>,
    /// a branch released its holds: upstream stores may collect
    pub(crate) unlinked: AtomicBool,
    /// the dataset's own store's current state, whose generation a linked branch shares
    /// cached blocks with
    main_current: Mutex<Weak<ArcSwap<Snapshot>>>,
    me: Weak<BranchSet>,
}

/// The generation holds that the generations of the branch rooted at `root` need: the
/// segments of the links of its current generation and of the older ones still on
/// disk. A generation numbered past `CURRENT` was never published, so its link holds
/// nothing. `None` when anything cannot be read, so that no hold is released.
fn link_holds_on_disk(root: &Path, id: uuid::Uuid) -> Option<Vec<GenHold>> {
    let current = std::fs::read_to_string(root.join("CURRENT")).ok()?;
    let current = commit::generation_number(current.trim());
    let mut holds = Vec::new();
    for (no, name, _, _) in crate::history::scan_generations(root, id).ok()? {
        if no > current {
            continue;
        }
        let Some(file) = link::read_link(&root.join(name)).ok()? else {
            continue;
        };
        for segment in file.segments {
            let hold = GenHold {
                branch_id: segment.branch_id,
                generation: segment.generation,
            };
            if !holds.contains(&hold) {
                holds.push(hold);
            }
        }
    }
    Some(holds)
}

/// Drop the holds of listed and retired branches that none of their generations on
/// disk need. A relink that crashed after recording its hold on the upstream's
/// generation, and before its branch's `CURRENT` named the relinked generation,
/// leaves such a hold, and the branch may never be opened again to release it.
/// Holds are only ever removed here. Returns whether any changed.
fn release_stale_holds(dir: &Path, table: &mut TableFile, broken: &HashSet<uuid::Uuid>) -> bool {
    let mut changed = false;
    for e in table.branches.iter_mut().chain(table.retired.iter_mut()) {
        if e.holds.is_empty() || broken.contains(&e.id) {
            continue;
        }
        let Some(needed) = link_holds_on_disk(&dir.join(e.id.to_string()), e.id) else {
            continue;
        };
        let before = e.holds.len();
        e.holds.retain(|h| needed.contains(h));
        if e.holds.len() != before {
            tracing::info!(target: "sparkles::store::branching",
                branch = e.name,
                released = before - e.holds.len(),
                "released upstream generation holds that no generation of the branch needs"
            );
            changed = true;
        }
    }
    changed
}

impl BranchSet {
    pub(crate) fn memory(
        dataset_id: uuid::Uuid,
        opts: &StoreOptions,
        cache: Arc<BlockCache>,
        quota: Arc<quota::Quota>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            memory: true,
            memory_bases: Default::default(),
            root: PathBuf::new(),
            dataset_id,
            opts: opts.clone(),
            cache,
            quota,
            table: Mutex::new(TableFile::new(dataset_id)),
            stores: Default::default(),
            opening: Mutex::new(()),
            merges: Default::default(),
            broken: Default::default(),
            unlinked: AtomicBool::new(false),
            main_current: Default::default(),
            me: me.clone(),
        })
    }

    fn save(&self, table: &TableFile) -> Result<()> {
        if self.memory {
            Ok(())
        } else {
            write_table(&self.root, table)
        }
    }

    /// The branch set of the dataset rooted at `root`, recovering from interrupted
    /// creations and deletions (see the crash-safety rules in F09).
    pub(crate) fn load(
        root: &Path,
        dataset_id: uuid::Uuid,
        opts: &StoreOptions,
        cache: Arc<BlockCache>,
        quota: Arc<quota::Quota>,
    ) -> Result<Arc<BranchSet>> {
        let table = match read_table(root)? {
            Some(t) if t.dataset_id != dataset_id => {
                return Err(Error::Corrupt(format!(
                    "{BRANCHES_FILE} belongs to dataset {}, not {dataset_id}",
                    t.dataset_id
                )));
            }
            Some(t) => t,
            None => TableFile::new(dataset_id),
        };
        let mut table = table;
        // retired branches nothing needs any more (a crash after the deletion that
        // freed them): their directories go below, as unlisted ones
        if !table.prune_retired().is_empty() {
            write_table(root, &table)?;
        }
        let mut broken = HashSet::new();
        let dir = root.join(BRANCHES_DIR);
        if dir.is_dir() {
            let listed: HashSet<String> = table.all().map(|e| e.id.to_string()).collect();
            for e in std::fs::read_dir(&dir)? {
                let e = e?;
                let name = e.file_name().to_string_lossy().into_owned();
                let path = e.path();
                if name.starts_with(".create-") || name.ends_with(".deleting") {
                    std::fs::remove_dir_all(&path)?;
                    continue;
                }
                if listed.contains(&name) {
                    continue;
                }
                // a branch whose deletion committed before its directory went
                match read_branch_file(&path) {
                    Ok(Some(b)) if b.dataset_id == dataset_id => {
                        std::fs::remove_dir_all(&path)?;
                    }
                    _ => tracing::warn!(target: "sparkles::store::branching",
                        "{}: not a branch of this dataset; left alone",
                        path.display()
                    ),
                }
            }
            sync_dir(&dir)?;
        }
        for e in table.all() {
            if !dir.join(e.id.to_string()).join(BRANCH_FILE).exists() {
                tracing::error!(target: "sparkles::store::branching",
                    "branch {} ({}) is listed but its directory is missing; its holds are kept",
                    e.name,
                    e.id
                );
                broken.insert(e.id);
                continue;
            }
            // a rename commits with the table: the identity file follows it
            if let Err(err) = sync_branch_file(&dir.join(e.id.to_string()), &e.name) {
                tracing::warn!(target: "sparkles::store::branching",
                    "branch {}: could not write its new name to {BRANCH_FILE}: {err}",
                    e.name
                );
            }
        }
        if release_stale_holds(&dir, &mut table, &broken) {
            write_table(root, &table)?;
        }
        Ok(Arc::new_cyclic(|me| BranchSet {
            memory: false,
            memory_bases: Default::default(),
            root: root.to_path_buf(),
            dataset_id,
            opts: opts.clone(),
            cache,
            quota,
            table: Mutex::new(table),
            stores: Default::default(),
            opening: Mutex::new(()),
            merges: Default::default(),
            broken: Mutex::new(broken),
            unlinked: AtomicBool::new(false),
            main_current: Mutex::new(Weak::new()),
            me: me.clone(),
        }))
    }

    pub(crate) fn table_len(&self) -> usize {
        self.table.lock().branches.len()
    }

    fn branch_root(&self, id: uuid::Uuid) -> PathBuf {
        self.root.join(BRANCHES_DIR).join(id.to_string())
    }

    /// The holds the branches place on the store `id`'s generations and commits.
    pub(crate) fn holds_on(&self, id: uuid::Uuid) -> Holds {
        let t = self.table.lock();
        holds_in(&t, id)
    }

    /// The current snapshot of the open store `id` (the dataset's own store for its
    /// id), or `None` when that store is not open.
    #[cfg(feature = "text")]
    pub(crate) fn current_of(&self, id: uuid::Uuid) -> Option<Arc<Snapshot>> {
        if id == self.dataset_id {
            self.main_current.lock().upgrade().map(|c| c.load_full())
        } else {
            self.stores.lock().get(&id).map(|s| s.snapshot())
        }
    }

    /// Remember the dataset's own store's current state (at its open).
    pub(crate) fn set_main(&self, current: &Arc<ArcSwap<Snapshot>>) {
        *self.main_current.lock() = Arc::downgrade(current);
    }

    /// The predicates exempt from conflicts in every merge of the dataset.
    pub(crate) fn exempt(&self) -> Vec<NamedNode> {
        self.table
            .lock()
            .exempt
            .iter()
            .filter_map(|p| NamedNode::new(p.clone()).ok())
            .collect()
    }

    pub(crate) fn main_protected(&self) -> bool {
        self.table.lock().main.protected
    }

    pub(crate) fn next_ordinal(&self) -> u64 {
        self.table.lock().next_ordinal as u64
    }

    /// Record the merges of store `id` (at its open) or one more (after a merge).
    pub(crate) fn note_merges(&self, id: uuid::Uuid, recs: &[MergeRec], replace: bool) {
        let mut m = self.merges.write();
        let v = m.entry(id).or_default();
        if replace {
            v.clear();
        }
        v.extend_from_slice(recs);
    }

    /// Give every open store of the dataset the holds the table places on it.
    fn refresh_holds(&self, main: &Store) {
        if self.memory {
            let t = self.table.lock();
            let needed: HashSet<_> = t
                .all()
                .flat_map(|e| [(e.id, e.from.seq), (e.from.branch_id, e.from.seq)])
                .collect();
            self.memory_bases
                .lock()
                .retain(|key, _| needed.contains(key));
        }
        // retired stores too: branches created from them read their files
        let stores: Vec<Arc<Store>> = self.stores.lock().values().cloned().collect();
        main.install_branch_holds(self);
        for s in stores {
            s.install_branch_holds(self);
        }
    }

    /// The open store of branch `id` (not `main`), opening it if needed.
    pub(crate) fn open_store(&self, id: uuid::Uuid) -> Result<Arc<Store>> {
        if let Some(s) = self.stores.lock().get(&id) {
            return Ok(s.clone());
        }
        let _opening = self.opening.lock();
        if let Some(s) = self.stores.lock().get(&id) {
            return Ok(s.clone());
        }
        if self.memory {
            // Memory branches are initialized before their table entry is published.
            // They have no directory from which a missing store could be reopened.
            return Err(branch::no_such_branch(&id.to_string()));
        }
        let (entry, next_ordinal, retired) = {
            let t = self.table.lock();
            let e = t
                .any_by_id(id)
                .cloned()
                .ok_or_else(|| branch::no_such_branch(&id.to_string()))?;
            let retired = t.by_id(id).is_none();
            (e, t.next_ordinal as u64, retired)
        };
        if self.broken.lock().contains(&id) {
            return Err(BranchError::error(
                BranchErrorKind::Gone,
                "branch-broken",
                format!("branch {}: its directory is missing", entry.name),
            ));
        }
        // the generations already open that the branch's link may share blocks with
        let mut share: Vec<Arc<Generation>> = self
            .stores
            .lock()
            .values()
            .map(|s| s.snapshot().generation.clone())
            .collect();
        if let Some(c) = self.main_current.lock().upgrade() {
            share.push(c.load().generation.clone());
        }
        let ctx = OpenCtx {
            ident: Ident {
                name: entry.name.clone(),
                ordinal: entry.ordinal,
                dataset_id: self.dataset_id,
                from: entry.from.into(),
            },
            set: self.me.clone(),
            cache: self.cache.clone(),
            quota: self.quota.clone(),
            // a retired branch takes no more commits
            protected: entry.protected || retired,
            next_ordinal,
            share,
        };
        let store = Arc::new(Store::open_branch(
            &self.branch_root(id),
            self.opts.clone(),
            ctx,
        )?);
        store.branching.retired.store(retired, Ordering::Relaxed);
        self.stores.lock().insert(id, store.clone());
        Ok(store)
    }

    /// The open stores of the listed branches (not `main`, nor retired branches).
    pub fn open_stores(&self) -> Vec<Arc<Store>> {
        self.stores
            .lock()
            .values()
            .filter(|s| !s.branching.retired.load(Ordering::Relaxed))
            .cloned()
            .collect()
    }

    /// The nearest listed branch that branch `id` descends from through starting
    /// points: its upstream, or the upstream of a retired branch it started from (the
    /// dataset id for `main`; `None` for `main` itself and unknown branches).
    pub(crate) fn upstream_of(&self, id: uuid::Uuid) -> Option<uuid::Uuid> {
        let t = self.table.lock();
        let mut cur = self.start_of(&t, id)?.branch_id;
        for _ in 0..1024 {
            if cur == self.dataset_id || t.by_id(cur).is_some() {
                return Some(cur);
            }
            cur = self.start_of(&t, cur)?.branch_id;
        }
        None
    }

    fn entry(&self, name: &str) -> Result<Entry> {
        self.table
            .lock()
            .by_name(name)
            .cloned()
            .ok_or_else(|| branch::no_such_branch(name))
    }

    /// The starting point of branch `id` (`None` for `main` and unknown branches).
    fn start_of(&self, t: &TableFile, id: uuid::Uuid) -> Option<CommitRef> {
        if id == self.dataset_id {
            return None;
        }
        t.any_by_id(id)
            .map(|e| e.from.into())
            .or_else(|| t.deleted.iter().find(|d| d.id == id).map(|d| d.from.into()))
    }

    /// The name of branch `id` (`None` when it was deleted).
    pub(crate) fn name_of(&self, id: uuid::Uuid) -> Option<String> {
        if id == self.dataset_id {
            return Some(MAIN.to_string());
        }
        self.table.lock().by_id(id).map(|e| e.name.clone())
    }

    pub(crate) fn named(&self, c: CommitRef) -> NamedCommitRef {
        NamedCommitRef {
            branch: self.name_of(c.branch_id),
            branch_id: c.branch_id,
            seq: c.seq,
        }
    }

    /// The commit `c` names, on the branch that made it: a seq at or below a branch's
    /// starting commit is a commit of its upstream.
    pub(crate) fn normalize(&self, c: CommitRef) -> CommitRef {
        let t = self.table.lock();
        let mut c = c;
        for _ in 0..1024 {
            match self.start_of(&t, c.branch_id) {
                Some(f) if c.seq <= f.seq => c.branch_id = f.branch_id,
                _ => break,
            }
        }
        c
    }

    /// The listed branches whose files can be read, in table order, each with its id,
    /// ordinal, starting commit and creation time (RFC 3339).
    pub(crate) fn readable_entries(&self) -> Vec<(String, uuid::Uuid, u16, CommitRef, String)> {
        let broken = self.broken.lock().clone();
        self.table
            .lock()
            .branches
            .iter()
            .filter(|e| !broken.contains(&e.id))
            .map(|e| {
                (
                    e.name.clone(),
                    e.id,
                    e.ordinal,
                    e.from.into(),
                    e.created.clone(),
                )
            })
            .collect()
    }

    /// The first-parent chain of commit `c`, from `main` to `c`'s branch: each branch
    /// with the range of its own commits on the chain, `(id, after, through)`.
    pub(crate) fn chain(&self, c: CommitRef) -> Vec<(uuid::Uuid, u64, u64)> {
        let c = self.normalize(c);
        let t = self.table.lock();
        let mut out = Vec::new();
        let mut cur = c;
        for _ in 0..1024 {
            match self.start_of(&t, cur.branch_id) {
                Some(f) => {
                    out.push((cur.branch_id, f.seq, cur.seq));
                    cur = f;
                    // a seq at or below the upstream's own start is inherited from further up
                    while let Some(ff) = self.start_of(&t, cur.branch_id) {
                        if cur.seq <= ff.seq {
                            cur.branch_id = ff.branch_id;
                        } else {
                            break;
                        }
                    }
                }
                None => {
                    out.push((cur.branch_id, 0, cur.seq));
                    break;
                }
            }
        }
        out.reverse();
        out
    }

    /// The merges branch `id` made, ensuring the branch's store is open so that its
    /// records are known (a record above the head is dropped at open).
    fn merges_of(&self, id: uuid::Uuid) -> Result<Vec<MergeRec>> {
        if id != self.dataset_id && self.table.lock().any_by_id(id).is_some() {
            self.open_store(id)?;
        }
        Ok(self.merges.read().get(&id).cloned().unwrap_or_default())
    }

    /// The frontiers of commits: for each, the newest own commit of every branch it
    /// descends from.
    pub(crate) fn frontiers(&self, cs: &[CommitRef]) -> Result<Vec<Frontier>> {
        let mut calc = FrontierCalc {
            set: self,
            merges: HashMap::new(),
            memo: HashMap::new(),
        };
        cs.iter()
            .map(|c| calc.frontier(self.normalize(*c)))
            .collect()
    }

    /// The merge bases of two commits: the common ancestors that no other common
    /// ancestor descends from.
    pub(crate) fn merge_bases(&self, a: CommitRef, b: CommitRef) -> Result<Vec<CommitRef>> {
        self.merge_bases_of(&[a], b)
    }

    /// Best common ancestors of a virtual commit with `parents`, and commit `b`.
    /// A virtual commit descends from every parent but has no persisted identity.
    pub(crate) fn merge_bases_of(
        &self,
        parents: &[CommitRef],
        b: CommitRef,
    ) -> Result<Vec<CommitRef>> {
        let mut calc = FrontierCalc {
            set: self,
            merges: HashMap::new(),
            memo: HashMap::new(),
        };
        let mut fa = Frontier::new();
        for a in parents {
            for (id, seq) in calc.frontier(self.normalize(*a))? {
                let current = fa.entry(id).or_insert(0);
                *current = (*current).max(seq);
            }
        }
        let fb = calc.frontier(self.normalize(b))?;
        let mut cands: Vec<CommitRef> = Vec::new();
        for (id, va) in fa.iter() {
            if let Some(vb) = fb.get(id) {
                let c = self.normalize(CommitRef {
                    branch_id: *id,
                    seq: (*va).min(*vb),
                });
                if !cands.contains(&c) {
                    cands.push(c);
                }
            }
        }
        let mut fronts = Vec::with_capacity(cands.len());
        for c in &cands {
            fronts.push(calc.frontier(*c)?);
        }
        let mut bases = Vec::new();
        for (i, c) in cands.iter().enumerate() {
            let dominated = cands.iter().enumerate().any(|(j, d)| {
                j != i && d != c && fronts[j].get(&c.branch_id).is_some_and(|v| *v >= c.seq)
            });
            if !dominated {
                bases.push(*c);
            }
        }
        bases.sort();
        Ok(bases)
    }

    /// Own commits of `a` that `b` does not descend from, counted over every branch of
    /// `a`'s frontier.
    pub(crate) fn ahead(&self, fa: &Frontier, fb: &Frontier) -> u64 {
        let t = self.table.lock();
        let mut n = 0;
        for (id, va) in fa {
            let floor = self.start_of(&t, *id).map_or(0, |f| f.seq);
            let seen = fb.get(id).copied().unwrap_or(0).max(floor);
            n += va.saturating_sub(seen);
        }
        n
    }
}

/// The holds on one store: its generations that branches read, and its commits that
/// branches keep readable.
pub(crate) type Holds = (Vec<(u32, Hold)>, Vec<(u64, Hold)>);

/// The holds of table `t` on store `id`.
fn holds_in(t: &TableFile, id: uuid::Uuid) -> Holds {
    let mut gens = Vec::new();
    let mut pins = Vec::new();
    for e in t.all() {
        for h in e.holds.iter().filter(|h| h.branch_id == id) {
            gens.push((
                commit::generation_number(&h.generation),
                Hold::Branch(e.name.clone()),
            ));
        }
        if let Some(b) = e.base_hold.filter(|b| b.branch_id == id) {
            pins.push((b.seq, Hold::BranchBase(e.name.clone())));
        }
    }
    (gens, pins)
}

/// A frontier: branch id → newest own commit descended from.
pub(crate) type Frontier = Map<uuid::Uuid, u64>;

/// Frontiers worked out without recursion: `cum(b, i)` is what branch `b`'s starting
/// point and its first `i` merges contribute.
struct FrontierCalc<'a> {
    set: &'a BranchSet,
    merges: HashMap<uuid::Uuid, Vec<MergeRec>>,
    memo: HashMap<(uuid::Uuid, usize), Arc<Frontier>>,
}

impl FrontierCalc<'_> {
    fn merges(&mut self, id: uuid::Uuid) -> Result<&Vec<MergeRec>> {
        if !self.merges.contains_key(&id) {
            let m = self.set.merges_of(id)?;
            self.merges.insert(id, m);
        }
        Ok(&self.merges[&id])
    }

    /// The key whose cumulative frontier covers commit `c` (normalized).
    fn key_of(&mut self, c: CommitRef) -> Result<(uuid::Uuid, usize)> {
        let i = self
            .merges(c.branch_id)?
            .partition_point(|m| m.seq <= c.seq);
        Ok((c.branch_id, i))
    }

    fn frontier(&mut self, c: CommitRef) -> Result<Frontier> {
        let key = self.key_of(c)?;
        let cum = self.cum(key)?;
        let mut f = (*cum).clone();
        bump(&mut f, c.branch_id, c.seq);
        Ok(f)
    }

    fn cum(&mut self, key: (uuid::Uuid, usize)) -> Result<Arc<Frontier>> {
        let mut stack = vec![key];
        let mut steps = 0u64;
        while let Some(&(b, i)) = stack.last() {
            steps += 1;
            if steps > 50_000_000 {
                return Err(Error::Corrupt(
                    "the merge records of the dataset's branches form a cycle".into(),
                ));
            }
            if self.memo.contains_key(&(b, i)) {
                stack.pop();
                continue;
            }
            if i == 0 {
                let from = {
                    let t = self.set.table.lock();
                    self.set.start_of(&t, b)
                };
                match from {
                    None => {
                        self.memo.insert((b, 0), Arc::new(Frontier::new()));
                        stack.pop();
                    }
                    Some(f) => {
                        let f = self.set.normalize(f);
                        let k = self.key_of(f)?;
                        match self.memo.get(&k) {
                            Some(cf) => {
                                let mut out = (**cf).clone();
                                bump(&mut out, f.branch_id, f.seq);
                                self.memo.insert((b, 0), Arc::new(out));
                                stack.pop();
                            }
                            None => stack.push(k),
                        }
                    }
                }
                continue;
            }
            let src = self.set.normalize(self.merges(b)?[i - 1].source);
            let k = self.key_of(src)?;
            let prev = self.memo.get(&(b, i - 1)).cloned();
            let other = self.memo.get(&k).cloned();
            match (prev, other) {
                (Some(p), Some(o)) => {
                    let mut out = (*p).clone();
                    for (id, v) in o.iter() {
                        bump(&mut out, *id, *v);
                    }
                    bump(&mut out, src.branch_id, src.seq);
                    self.memo.insert((b, i), Arc::new(out));
                    stack.pop();
                }
                (p, o) => {
                    if p.is_none() {
                        stack.push((b, i - 1));
                    }
                    if o.is_none() {
                        stack.push(k);
                    }
                }
            }
        }
        Ok(self.memo[&key].clone())
    }
}

fn bump(f: &mut Frontier, id: uuid::Uuid, seq: u64) {
    let e = f.entry(id).or_insert(seq);
    *e = (*e).max(seq);
}

// ------------------------------------------------------------ store methods ------

/// A branch's store: the dataset's own store for `main`, an open branch store
/// otherwise.
pub enum BranchStore<'a> {
    Main(&'a Store),
    Other(Arc<Store>),
}

impl std::ops::Deref for BranchStore<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        match self {
            BranchStore::Main(s) => s,
            BranchStore::Other(s) => s,
        }
    }
}

impl BranchStore<'_> {
    /// The branch store as an `Arc` (`None` for `main`).
    pub fn shared(&self) -> Option<Arc<Store>> {
        match self {
            BranchStore::Main(_) => None,
            BranchStore::Other(s) => Some(s.clone()),
        }
    }
}

impl Store {
    /// The name of this store's branch (`main` for a dataset's own store).
    pub fn branch_name(&self) -> String {
        match &self.branching.ident {
            Some(_) => self.branching.name.read().clone(),
            None => MAIN.to_string(),
        }
    }

    /// The id of this store's branch: the dataset id for `main`.
    pub fn branch_id(&self) -> uuid::Uuid {
        self.dataset_id
    }

    /// The id of the dataset the store belongs to (for a branch store, the dataset
    /// whose branch it is; [`dataset_id`](Self::dataset_id) is then the branch id).
    pub fn owner_dataset_id(&self) -> uuid::Uuid {
        self.branching
            .ident
            .as_ref()
            .map_or(self.dataset_id, |i| i.dataset_id)
    }

    /// Whether this is a branch store (not a dataset's own store).
    pub fn is_branch(&self) -> bool {
        self.branching.ident.is_some()
    }

    /// The branch's ordinal (0 for `main`).
    pub fn branch_ordinal(&self) -> u16 {
        self.branching.ident.as_ref().map_or(0, |i| i.ordinal)
    }

    /// The commit a branch store started from (`None` for `main`).
    pub fn branch_from(&self) -> Option<CommitRef> {
        self.branching.ident.as_ref().map(|i| i.from)
    }

    /// Whether the branch refuses writes other than merges.
    pub fn branch_protected(&self) -> bool {
        self.branching.protected.load(Ordering::Relaxed)
    }

    /// `BranchProtected` for a commit of `kind` on a protected branch: merges and the
    /// embedding worker's commits pass.
    pub(crate) fn check_protected(&self, kind: CommitKind) -> Result<()> {
        if self.branch_protected() && !matches!(kind, CommitKind::Merge | CommitKind::Embed) {
            return Err(branch::protected(&self.branch_name()));
        }
        Ok(())
    }

    /// The merge record of commit `seq` of this store.
    pub(crate) fn merge_record(&self, seq: u64) -> Option<MergeRec> {
        self.branching.merges.lock().get(seq)
    }

    /// The branch set, if this is a dataset's own persistent store.
    pub(crate) fn owned_set(&self) -> Result<&Arc<BranchSet>> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        match &self.branching.set {
            SetRef::Owner(s) => Ok(s),
            SetRef::Member(_) => Err(branch::invalid_branch(
                "branch operations are made through the dataset's main store",
            )),
            SetRef::None => Err(BranchError::error(
                BranchErrorKind::Unsupported,
                "branches-unsupported",
                "branches need a persistent dataset",
            )),
        }
    }

    /// The branch set this store belongs to, if any.
    pub fn branch_set(&self) -> Option<Arc<BranchSet>> {
        crate::sparql::extensions::assert_family(self.owner_dataset_id());
        self.branching.set()
    }

    /// Put the holds that `set`'s table places on this store into its history state.
    pub(crate) fn install_branch_holds(&self, set: &BranchSet) {
        if let Some(mem) = &self.mem_history {
            let t = set.table.lock();
            let needed: HashSet<u64> = t
                .all()
                .filter_map(|e| e.base_hold)
                .filter(|b| b.branch_id == self.dataset_id)
                .map(|b| b.seq)
                .chain(self.branching.ident.as_ref().map(|i| i.from.seq))
                .collect();
            let bases = set.memory_bases.lock();
            let mut mem = mem.lock();
            mem.branch_bases = bases
                .iter()
                .filter(|((id, seq), _)| *id == self.dataset_id && needed.contains(seq))
                .map(|((_, seq), b)| (*seq, b.clone()))
                .collect();
            return;
        }
        let Some(h) = &self.history else { return };
        let (gens, pins) = set.holds_on(self.dataset_id);
        let mut h = h.lock();
        h.branch_gens = gens;
        h.branch_pins = pins;
    }

    /// The store of branch `name`: this store for `main`, the open branch store
    /// otherwise.
    pub fn branch(&self, name: &str) -> Result<BranchStore<'_>> {
        if name == MAIN {
            return Ok(BranchStore::Main(self));
        }
        let set = self.owned_set()?;
        let e = set.entry(name)?;
        Ok(BranchStore::Other(set.open_store(e.id)?))
    }

    /// The store of branch id `id`.
    pub(crate) fn branch_by_id(&self, id: uuid::Uuid) -> Result<BranchStore<'_>> {
        if id == self.dataset_id {
            return Ok(BranchStore::Main(self));
        }
        let set = self.owned_set()?;
        if set.table.lock().any_by_id(id).is_none() {
            return Err(BranchError::error(
                BranchErrorKind::Gone,
                "merge-base-gone",
                format!("branch {id} was deleted"),
            ));
        }
        Ok(BranchStore::Other(set.open_store(id)?))
    }

    /// The id of branch `name`.
    pub fn branch_id_of(&self, name: &str) -> Result<uuid::Uuid> {
        if name == MAIN {
            return Ok(self.dataset_id);
        }
        Ok(self.owned_set()?.entry(name)?.id)
    }

    /// The number of branches, `main` included.
    pub fn branch_count(&self) -> usize {
        match &self.branching.set {
            SetRef::Owner(s) => 1 + s.table.lock().branches.len(),
            _ => 1,
        }
    }

    /// Every branch, `main` first, then by name.
    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        let set = self.owned_set()?;
        let mut names: Vec<String> = set
            .table
            .lock()
            .branches
            .iter()
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        let mut out = vec![self.branch_info(MAIN)?];
        for n in names {
            match self.branch_info(&n) {
                Ok(i) => out.push(i),
                Err(e) if matches!(&e, Error::Branch(b) if b.code == "no-such-branch") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// One branch.
    pub fn branch_info(&self, name: &str) -> Result<BranchInfo> {
        let set = self.owned_set()?;
        if name == MAIN {
            let t = set.table.lock();
            let created_ms = commit::read_dataset_created(self.root.as_deref())
                .unwrap_or_else(|| self.catalog.lock().first().map_or(0, |c| c.timestamp_ms));
            return Ok(BranchInfo {
                name: MAIN.into(),
                id: self.dataset_id,
                ordinal: 0,
                head: Some(self.head_commit()),
                from: None,
                upstream: None,
                merge_base: None,
                ahead: 0,
                behind: 0,
                protected: t.main.protected,
                note: t.main.note.clone(),
                created_ms,
                storage: BranchStorage {
                    linked: false,
                    own_bytes: self.root.as_ref().map_or(0, |r| {
                        dir_size(r).saturating_sub(dir_size(&r.join(BRANCHES_DIR)))
                    }),
                    held_bytes: 0,
                    generation: self.snapshot().generation.name.clone(),
                },
                broken: false,
                scratch: None,
            });
        }
        let e = set.entry(name)?;
        let upstream_id = set.upstream_of(e.id);
        let upstream_name = upstream_id.and_then(|u| set.name_of(u));
        let created_ms = commit::parse_rfc3339_ms(&e.created).unwrap_or(0);
        let mut info = BranchInfo {
            name: e.name.clone(),
            id: e.id,
            ordinal: e.ordinal,
            head: None,
            from: Some(set.named(e.from.into())),
            upstream: upstream_name.clone(),
            merge_base: None,
            ahead: 0,
            behind: 0,
            protected: e.protected,
            note: e.note.clone(),
            created_ms,
            storage: BranchStorage {
                linked: !e.holds.is_empty(),
                own_bytes: if set.memory {
                    0
                } else {
                    dir_size(&set.branch_root(e.id))
                },
                held_bytes: 0,
                generation: String::new(),
            },
            broken: set.broken.lock().contains(&e.id),
            scratch: e.scratch.clone(),
        };
        if info.broken {
            return Ok(info);
        }
        let store = set.open_store(e.id)?;
        let head = store.head_commit();
        info.head = Some(head);
        info.storage.generation = store.snapshot().generation.name.clone();
        info.storage.held_bytes = e
            .holds
            .iter()
            .map(|h| {
                let dir = if h.branch_id == self.dataset_id {
                    self.root.as_ref().map(|r| r.join(&h.generation))
                } else {
                    Some(set.branch_root(h.branch_id).join(&h.generation))
                };
                let current = dir.as_ref().and_then(|d| d.parent()).and_then(|p| {
                    std::fs::read_to_string(p.join("CURRENT"))
                        .ok()
                        .map(|c| c.trim().to_string())
                });
                if current.as_deref() == Some(h.generation.as_str()) {
                    0
                } else {
                    dir.map_or(0, |d| dir_size(&d))
                }
            })
            .sum();
        // relative to the upstream, if it still exists
        if let Some(up) = &upstream_name {
            let up_head = self.branch(up)?.head_commit().seq;
            let me = CommitRef {
                branch_id: e.id,
                seq: head.seq,
            };
            let them = CommitRef {
                branch_id: upstream_id.unwrap_or(e.from.branch_id),
                seq: up_head,
            };
            let f = set.frontiers(&[me, them])?;
            info.ahead = set.ahead(&f[0], &f[1]);
            info.behind = set.ahead(&f[1], &f[0]);
            info.merge_base = set.merge_bases(me, them)?.first().map(|c| set.named(*c));
        }
        Ok(info)
    }

    /// The bytes of this store's own directory: a branch's, or for `main` the
    /// dataset's less its branches.
    pub fn branch_own_bytes(&self) -> u64 {
        match (&self.root, &self.branching.ident) {
            (Some(r), Some(_)) => dir_size(r),
            (Some(r), None) => dir_size(r).saturating_sub(dir_size(&r.join(BRANCHES_DIR))),
            _ => 0,
        }
    }

    /// The bytes the dataset keeps on disk only for its branches: generations that are
    /// no longer current where they were built and that a branch's link still reads,
    /// and the directories of retired branches (0 without branches).
    pub fn branch_held_bytes(&self) -> u64 {
        let Ok(set) = self.owned_set() else { return 0 };
        if set.memory {
            return 0;
        }
        let t = set.table.lock().clone();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for e in t.all() {
            for h in &e.holds {
                let dir = if h.branch_id == set.dataset_id {
                    set.root.join(&h.generation)
                } else {
                    set.branch_root(h.branch_id).join(&h.generation)
                };
                let current = dir.parent().and_then(|p| {
                    std::fs::read_to_string(p.join("CURRENT"))
                        .ok()
                        .map(|c| c.trim().to_string())
                });
                if current.as_deref() != Some(h.generation.as_str()) {
                    dirs.insert(dir);
                }
            }
        }
        for r in &t.retired {
            dirs.insert(set.branch_root(r.id));
        }
        // a held generation inside a retired branch's directory counts once
        let retired: Vec<PathBuf> = t.retired.iter().map(|r| set.branch_root(r.id)).collect();
        dirs.iter()
            .filter(|d| !retired.iter().any(|r| d.starts_with(r) && *d != r))
            .map(|d| dir_size(d))
            .sum()
    }

    /// Change a branch's protection (any branch, `main` included).
    pub fn set_branch_protected(&self, name: &str, on: bool) -> Result<BranchInfo> {
        self.update_entry(name, |m, e| match e {
            Some(e) => e.protected = on,
            None => m.protected = on,
        })?;
        self.branch_info(name)
    }

    /// The predicates whose cells never conflict in this dataset's merges: both sides'
    /// changes to them are kept, as with the quad scope.
    pub fn merge_exempt(&self) -> Result<Vec<NamedNode>> {
        Ok(self.owned_set()?.exempt())
    }

    /// Set the predicates exempt from conflicts in this dataset's merges (empty: none).
    pub fn set_merge_exempt(&self, predicates: &[NamedNode]) -> Result<Vec<NamedNode>> {
        if predicates.len() > 1024 {
            return Err(branch::invalid_merge("at most 1024 exempt predicates"));
        }
        let set = self.owned_set()?;
        let mut t = set.table.lock();
        let mut next = t.clone();
        next.exempt = predicates.iter().map(|p| p.as_str().to_string()).collect();
        next.exempt.sort();
        next.exempt.dedup();
        set.save(&next)?;
        *t = next;
        drop(t);
        Ok(set.exempt())
    }

    /// Change a branch's note (`None` removes it).
    pub fn set_branch_note(&self, name: &str, note: Option<String>) -> Result<BranchInfo> {
        if note.as_ref().is_some_and(|n| n.len() > 1024) {
            return Err(branch::invalid_branch("the note is longer than 1024 bytes"));
        }
        self.update_entry(name, |m, e| match e {
            Some(e) => e.note = note.clone(),
            None => m.note = note.clone(),
        })?;
        self.branch_info(name)
    }

    /// Mark a branch as a scratch branch, or clear the mark (`None`). `main` is never
    /// a scratch branch.
    pub fn set_branch_scratch(
        &self,
        name: &str,
        scratch: Option<branch::Scratch>,
    ) -> Result<BranchInfo> {
        if name == MAIN {
            return Err(branch::invalid_branch("main cannot be a scratch branch"));
        }
        self.update_entry(name, |_, e| {
            if let Some(e) = e {
                e.scratch = scratch.clone();
            }
        })?;
        self.branch_info(name)
    }

    fn update_entry(
        &self,
        name: &str,
        f: impl Fn(&mut MainEntry, Option<&mut Entry>),
    ) -> Result<()> {
        let set = self.owned_set()?;
        let mut t = set.table.lock();
        let mut next = t.clone();
        if name == MAIN {
            f(&mut next.main, None);
        } else {
            let mut main = next.main.clone();
            let e = next
                .branches
                .iter_mut()
                .find(|e| e.name == name)
                .ok_or_else(|| branch::no_such_branch(name))?;
            f(&mut main, Some(e));
        }
        set.save(&next)?;
        *t = next;
        let protected_main = t.main.protected;
        let by_id: HashMap<uuid::Uuid, bool> =
            t.branches.iter().map(|e| (e.id, e.protected)).collect();
        drop(t);
        self.branching
            .protected
            .store(protected_main, Ordering::Relaxed);
        for s in set.open_stores() {
            if let Some(p) = by_id.get(&s.dataset_id) {
                s.branching.protected.store(*p, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Create branch `name` from a commit of another branch. The new branch shares the
    /// index files of the generation that holds the commit, so this writes a few
    /// kilobytes whatever the dataset's size. When the link would chain more than
    /// [`StoreOptions::max_branch_depth`] log segments, the branch's index is built
    /// instead.
    pub fn create_branch(&self, name: &str, o: &BranchOptions) -> Result<BranchInfo> {
        branch::check_name(name)?;
        if o.note.as_ref().is_some_and(|n| n.len() > 1024) {
            return Err(branch::invalid_branch("the note is longer than 1024 bytes"));
        }
        let set = self.owned_set()?.clone();
        if set.memory {
            return self.create_memory_branch(name, o, &set);
        }
        let root = set.root.clone();
        {
            let t = set.table.lock();
            if t.by_name(name).is_some() {
                return Err(branch::conflict(
                    "branch-exists",
                    format!("branch {name} exists"),
                ));
            }
            if 1 + t.branches.len() >= self.opts.max_branches {
                return Err(branch::conflict(
                    "branch-limit",
                    format!(
                        "the dataset has {} branches, the most allowed",
                        1 + t.branches.len()
                    ),
                ));
            }
            if t.next_ordinal > u16::MAX as u32 {
                return Err(branch::conflict(
                    "branch-limit",
                    "the dataset has used all 65,535 branch ordinals",
                ));
            }
        }
        let up = self.branch(&o.from)?;
        let id = uuid::Uuid::new_v4();
        // under the upstream's writer lock: the starting commit, where it ends in the
        // log, the delta vocabulary's length, and the holds (in memory first)
        let start = up.capture_start(&o.at)?;
        let (start, up_store): (Start, BranchStore<'_>) = match start {
            Captured::Here(s) => (s, up),
            Captured::Inherited(c) => {
                // the commit is one of an upstream's: start from that branch
                let owner = self.branch_by_id(c.branch_id)?;
                match owner.capture_start(&At::Commit(c.seq))? {
                    Captured::Here(s) => (s, owner),
                    Captured::Inherited(_) => {
                        return Err(Error::Corrupt(
                            "a branch's starting commit resolves to itself".into(),
                        ));
                    }
                }
            }
        };
        let build = start.segments.len() > self.opts.max_branch_depth;
        let entry = {
            let mut t = set.table.lock();
            if t.by_name(name).is_some() {
                return Err(branch::conflict(
                    "branch-exists",
                    format!("branch {name} exists"),
                ));
            }
            let ordinal = t.next_ordinal as u16;
            let e = Entry {
                name: name.to_string(),
                id,
                ordinal,
                from: FromRef {
                    branch_id: up_store.dataset_id,
                    seq: start.commit.seq,
                },
                created: commit::rfc3339_ms(self.now_ms()),
                protected: o.protected,
                note: o.note.clone(),
                holds: if build {
                    Vec::new()
                } else {
                    start
                        .segments
                        .iter()
                        .map(|s| GenHold {
                            branch_id: s.branch_id,
                            generation: s.generation.clone(),
                        })
                        .collect()
                },
                base_hold: Some(FromRef {
                    branch_id: up_store.dataset_id,
                    seq: start.commit.seq,
                }),
                scratch: None,
            };
            t.next_ordinal += 1;
            t.branches.push(e.clone());
            e
        };
        set.refresh_holds(self);
        let undo = |set: &BranchSet| {
            let mut t = set.table.lock();
            t.branches.retain(|e| e.id != id);
            drop(t);
            set.refresh_holds(self);
        };
        let r = self.write_branch_dir(&root, &set, &entry, &start, &up_store, build);
        if let Err(e) = r {
            undo(&set);
            return Err(e);
        }
        // the commit point: the table names the branch
        let t = set.table.lock().clone();
        if let Err(e) = write_table(&root, &t) {
            undo(&set);
            let _ = std::fs::remove_dir_all(set.branch_root(id));
            return Err(e);
        }
        self.failpoint("branch-create-committed");
        self.branching
            .next_ordinal
            .fetch_max(t.next_ordinal as u64, Ordering::Relaxed);
        for s in set.open_stores() {
            s.branching
                .next_ordinal
                .fetch_max(t.next_ordinal as u64, Ordering::Relaxed);
        }
        drop(up_store);
        tracing::info!(target: "sparkles::store::branching",
            branch = name,
            id = %id,
            from = o.from,
            seq = start.commit.seq,
            linked = !build,
            "created a branch"
        );
        self.branch_info(name)
    }

    fn create_memory_branch(
        &self,
        name: &str,
        o: &BranchOptions,
        set: &Arc<BranchSet>,
    ) -> Result<BranchInfo> {
        let (snap, resolved) = self.branch_snapshot_at(&o.from, &o.at, &Default::default())?;
        let (_, owner_id) = self.branch_resolve(&o.from, &At::Commit(resolved.commit.seq))?;
        let up = self.branch_by_id(owner_id)?;
        let admit = |t: &TableFile, ordinal: bool| {
            if t.by_name(name).is_some() {
                return Err(branch::conflict(
                    "branch-exists",
                    format!("branch {name} exists"),
                ));
            }
            if 1 + t.branches.len() >= self.opts.max_branches
                || (ordinal && t.next_ordinal > u16::MAX as u32)
            {
                return Err(branch::conflict(
                    "branch-limit",
                    "the dataset has used its branch limit",
                ));
            }
            Ok(())
        };
        // Reserve the ordinal even if index configuration fails. The table lock is not
        // held while the store and its indexes are built, so readers of the table do
        // not wait for text, spatial or vector builds. No branch is published until
        // all fallible initialization has completed.
        let (ordinal, next) = {
            let mut t = set.table.lock();
            admit(&t, true)?;
            let ordinal = t.next_ordinal as u16;
            t.next_ordinal += 1;
            (ordinal, t.next_ordinal)
        };
        self.failpoint("memory-ordinal-reserved");
        self.branching
            .next_ordinal
            .fetch_max(next as u64, Ordering::Relaxed);
        for s in set.open_stores() {
            s.branching
                .next_ordinal
                .fetch_max(next as u64, Ordering::Relaxed);
        }
        let id = uuid::Uuid::new_v4();
        let from = FromRef {
            branch_id: owner_id,
            seq: resolved.commit.seq,
        };
        let root = CommitInfo {
            generation: 0,
            ..resolved.commit
        };
        let gen_ = Arc::new(Generation::memory_branch(&snap.generation, snap.dvocab_len));
        let mut store = Store::in_memory_from(
            self.opts.clone(),
            gen_,
            branch::bnode_range_start(ordinal),
            up.prefixes(),
            root,
            id,
            None,
        );
        store.cache = set.cache.clone();
        store.quota = set.quota.clone();
        store.branching.ident = Some(Ident {
            name: name.into(),
            ordinal,
            dataset_id: self.dataset_id,
            from: from.into(),
        });
        *store.branching.name.write() = name.into();
        store.branching.set = SetRef::Member(set.me.clone());
        store
            .branching
            .protected
            .store(o.protected, Ordering::Relaxed);
        store
            .guard_required
            .store(up.guard_required.load(Ordering::Relaxed), Ordering::Relaxed);
        let mut state = (*store.snapshot()).clone();
        state.dataset_id = self.owner_dataset_id();
        state.delta = snap.delta.clone();
        state.version = snap.version;
        state.commit = root.seq;
        state.cache = set.cache.clone();
        store.current.store(Arc::new(state));
        self.failpoint("memory-branch-indexes");
        up.configure_indexes(&store)?;
        *store.describe.write() = up.describe.read().clone();
        *store.compaction.settings.lock() = up.compaction.settings.lock().clone();
        if let (Some(src), Some(dst)) = (&up.mem_history, &store.mem_history) {
            dst.lock().retention = src.lock().retention;
        }
        let mut t = set.table.lock();
        // another creation may have taken the name or the last slot meanwhile
        admit(&t, false)?;
        let next = next.max(t.next_ordinal);
        t.branches.push(Entry {
            name: name.into(),
            id,
            ordinal,
            from,
            created: commit::rfc3339_ms(self.now_ms()),
            protected: o.protected,
            note: o.note.clone(),
            holds: Vec::new(),
            base_hold: Some(from),
            scratch: None,
        });
        let store = Arc::new(store);
        let mut bases = set.memory_bases.lock();
        bases.insert((owner_id, root.seq), (resolved.commit, snap));
        bases.insert((id, root.seq), (root, store.snapshot()));
        drop(bases);
        set.stores.lock().insert(id, store.clone());
        drop(t);
        self.branching
            .next_ordinal
            .fetch_max(next as u64, Ordering::Relaxed);
        for s in set.open_stores() {
            s.branching
                .next_ordinal
                .fetch_max(next as u64, Ordering::Relaxed);
        }
        set.refresh_holds(self);
        self.branch_info(name)
    }

    /// Write a new branch's directory under `branches/.create-<id>/`, then rename it
    /// into place.
    fn write_branch_dir(
        &self,
        root: &Path,
        set: &BranchSet,
        e: &Entry,
        start: &Start,
        up: &Store,
        build: bool,
    ) -> Result<()> {
        let dir = root.join(BRANCHES_DIR);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".create-{}", e.id));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;
        let r = (|| -> Result<()> {
            let bf = BranchFile {
                format: 1,
                dataset_id: self.dataset_id,
                name: e.name.clone(),
                id: e.id,
                ordinal: e.ordinal,
                from: e.from,
                created: e.created.clone(),
            };
            write_synced(
                &tmp.join(BRANCH_FILE),
                &serde_json::to_vec_pretty(&bf).expect("serializes"),
            )?;
            write_synced(
                &tmp.join("dataset.json"),
                &commit::dataset_file_bytes(e.id, "branch", self.now_ms()),
            )?;
            // the first generation is gen-0001, linked or built (0 names in-memory ones)
            let gen_no: u32 = 1;
            let gen_name = format!("gen-{gen_no:04}");
            let gdir = tmp.join(&gen_name);
            std::fs::create_dir_all(&gdir)?;
            let base = CommitInfo {
                generation: gen_no,
                ..start.commit
            };
            if build {
                let snap = start.snapshot.clone().expect("a build has the state");
                let meta = up.build_from_snapshot(
                    &gdir,
                    &snap,
                    None,
                    branch::bnode_range_start(e.ordinal),
                    self.opts.min_free_disk_bytes,
                    up.prefixes(),
                    |_| Ok(true),
                    || {},
                )?;
                let _ = meta;
            } else {
                let link = LinkFile {
                    format: 1,
                    base_seq: start.commit.seq,
                    segments: start.segments.clone(),
                    overlay: None,
                };
                write_synced(
                    &gdir.join(LINK_FILE),
                    &serde_json::to_vec_pretty(&link).expect("serializes"),
                )?;
                write_synced(&gdir.join("delta.vocab"), &[])?;
            }
            write_synced(&gdir.join("wal.log"), &[])?;
            write_synced(
                &gdir.join("commit.json"),
                &commit::gen_commit_bytes(e.id, "branch", &base),
            )?;
            sync_dir(&gdir)?;
            write_synced(&tmp.join("CURRENT"), gen_name.as_bytes())?;
            Catalog::create(&tmp.join("commits.bin"), e.id, base)?;
            let (retention, horizon) = match &up.history {
                Some(h) => {
                    let h = h.lock();
                    (h.retention, h.catalog)
                }
                None => Default::default(),
            };
            crate::history::write_file(&tmp, e.id, &Default::default(), retention, &[], horizon)?;
            write_synced(
                &tmp.join("prefixes.json"),
                &serde_json::to_vec_pretty(&up.prefixes()).expect("serializes"),
            )?;
            if let Some(up_root) = up.root() {
                for f in COPIED_FILES {
                    let src = up_root.join(f);
                    let Ok(mut bytes) = std::fs::read(&src) else {
                        continue;
                    };
                    // files that name their dataset name the branch
                    if f.ends_with(".json")
                        && let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && let Some(obj) = v.as_object_mut()
                        && obj.contains_key("datasetId")
                    {
                        obj.insert("datasetId".into(), e.id.to_string().into());
                        bytes = serde_json::to_vec_pretty(&v).expect("serializes");
                    }
                    write_synced(&tmp.join(f), &bytes)?;
                }
            }
            sync_dir(&tmp)?;
            Ok(())
        })();
        if let Err(err) = r {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(err);
        }
        self.failpoint("branch-create-built");
        let _ = set;
        std::fs::rename(&tmp, dir.join(e.id.to_string()))?;
        sync_dir(&dir)?;
        self.failpoint("branch-create-renamed");
        Ok(())
    }

    /// Capture where a new branch starts on this store: the commit `at` names, the log
    /// segments that lead to it, and the delta vocabulary's length. The upstream holds
    /// are registered by the caller before the writer lock is released.
    fn capture_start(&self, at: &At) -> Result<Captured> {
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(BranchError::error(
                BranchErrorKind::Unsupported,
                "branches-unsupported",
                "branches need a persistent dataset",
            ));
        };
        let w = self.guarded_writer();
        let head = w.head;
        let r = match self.resolve_with(at, head) {
            Err(e) => match branch::inherited_commit(&e) {
                Some(c) => return Ok(Captured::Inherited(c)),
                None => return Err(e),
            },
            Ok(r) => r,
        };
        let seq = r.commit.seq;
        if let Some(i) = &self.branching.ident
            && seq < i.from.seq
        {
            return Ok(Captured::Inherited(CommitRef {
                branch_id: i.from.branch_id,
                seq,
            }));
        }
        let live = self.snapshot();
        let current = commit::generation_number(&live.generation.name);
        let mut h = hist.lock();
        let Some(owner) = h.owner(seq, current, head.seq) else {
            return Err(self.history_gone(&h, seq, head.seq, None, Some(r.commit)));
        };
        let entry = h.gens[&owner].clone();
        let generation = self.history_generation(&mut h, owner, current, &live, &entry)?;
        if owner == current {
            generation.dvocab.sync()?;
        }
        let point = wal_point(&generation, &entry, seq)?;
        let ds_root = match &self.branching.ident {
            Some(_) => link::dataset_root_of(root)?,
            None => root.clone(),
        };
        let rel = entry
            .dir
            .strip_prefix(&ds_root)
            .map_err(|_| Error::Corrupt(format!("{} is outside the dataset", entry.dir.display())))?
            .to_string_lossy()
            .into_owned();
        let seg = Segment {
            branch_id: self.dataset_id,
            generation: entry.name.clone(),
            path: rel,
            base_seq: entry.base.seq,
            end_seq: seq,
            wal_end: point.offset,
            dvocab_len: generation.dvocab.len(),
        };
        let mut segments = generation
            .linked()
            .map(|l| l.file.segments.clone())
            .unwrap_or_default();
        segments.push(seg);
        let snapshot = if segments.len() > self.opts.max_branch_depth {
            drop(h);
            drop(w);
            Some(self.snapshot_at(&At::Commit(seq), &Default::default())?.0)
        } else {
            None
        };
        Ok(Captured::Here(Start {
            commit: r.commit,
            segments,
            snapshot,
        }))
    }

    /// Delete branch `name`: its name, commits, snapshots and storage. Refused while
    /// other branches start from it, and, unless `force`, while it has commits its
    /// upstream does not descend from.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        self.delete_branch_with(
            name,
            &branch::DeleteOptions {
                force,
                reparent: false,
            },
        )
    }

    /// Delete branch `name`. With `o.reparent`, a branch that other branches were
    /// created from is deleted too: its name goes, and the branches created from it
    /// take its upstream as theirs. Its storage stays, *retired*, as long as their
    /// history and linked generations need it. Without `o.force`, the deletion is
    /// refused while the branch has commits that neither its upstream descends from nor
    /// a re-parented branch keeps in its history.
    pub fn delete_branch_with(&self, name: &str, o: &branch::DeleteOptions) -> Result<()> {
        if name == MAIN {
            return Err(branch::invalid_branch("branch main cannot be deleted"));
        }
        let set = self.owned_set()?.clone();
        let root = set.root.clone();
        let e = set.entry(name)?;
        // the branches that start from it or read its files
        let (children, kept) = {
            let t = set.table.lock();
            let children: Vec<String> = t
                .all()
                .filter(|c| {
                    c.id != e.id
                        && (c.from.branch_id == e.id || c.holds.iter().any(|h| h.branch_id == e.id))
                })
                .map(|c| c.name.clone())
                .collect();
            // the newest of its commits a child's history keeps
            let kept = t
                .all()
                .filter(|c| c.from.branch_id == e.id)
                .map(|c| c.from.seq)
                .max()
                .unwrap_or(0);
            (children, kept)
        };
        if !children.is_empty() && !o.reparent {
            return Err(branch::conflict(
                "has-children",
                format!(
                    "branch {} was created from {name} or reads its files; delete it first, or re-parent it",
                    children[0]
                ),
            ));
        }
        if !o.force && !set.broken.lock().contains(&e.id) {
            let store = set.open_store(e.id)?;
            let head = store.head_commit().seq;
            if head > e.from.seq {
                let up = set.upstream_of(e.id).unwrap_or(self.dataset_id);
                let up_head = self.branch_by_id(up)?.head_commit().seq;
                let f = set.frontiers(&[CommitRef {
                    branch_id: up,
                    seq: up_head,
                }])?;
                let merged = f[0].get(&e.id).copied().unwrap_or(0);
                let safe = merged.max(kept).max(e.from.seq);
                if safe < head {
                    return Err(branch::conflict(
                        "unmerged",
                        format!(
                            "branch {name} has {} its upstream does not have; merge it or delete with force",
                            match head - safe {
                                1 => "1 commit".to_string(),
                                n => format!("{n} commits"),
                            }
                        ),
                    ));
                }
            }
        }
        let retire = !children.is_empty();
        // the commit point: the table without the name
        let mut t = set.table.lock();
        let mut next = t.clone();
        next.branches.retain(|b| b.id != e.id);
        if retire {
            next.retired.push(e.clone());
        } else {
            next.deleted.push(Tombstone {
                name: e.name.clone(),
                id: e.id,
                ordinal: e.ordinal,
                from: e.from,
            });
        }
        let pruned = next.prune_retired();
        set.save(&next)?;
        *t = next;
        drop(t);
        self.failpoint("branch-delete-committed");
        if retire {
            // its store stays for the branches created from it, read-only
            if let Some(s) = set.stores.lock().get(&e.id) {
                s.branching.retired.store(true, Ordering::Relaxed);
                s.branching.protected.store(true, Ordering::Relaxed);
            }
        }
        let mut gone: Vec<uuid::Uuid> = pruned;
        if !retire {
            gone.insert(0, e.id);
        }
        for id in &gone {
            let store = set.stores.lock().remove(id);
            set.merges.write().remove(id);
            set.broken.lock().remove(id);
            if let Some(mem) = store.as_ref().and_then(|s| s.mem_history.as_ref()) {
                mem.lock().branch_bases.clear();
            }
            drop(store);
        }
        set.refresh_holds(self);
        for id in gone.iter().filter(|_| !set.memory) {
            let dir = set.branch_root(*id);
            let doomed = dir.with_extension("deleting");
            if dir.exists() {
                std::fs::rename(&dir, &doomed)?;
                sync_dir(&root.join(BRANCHES_DIR))?;
                self.failpoint("branch-delete-renamed");
                std::fs::remove_dir_all(&doomed)?;
            }
        }
        // the generations only the branch held can go
        let head = self.head_commit().seq;
        self.collect_history(
            commit::generation_number(&self.snapshot().generation.name),
            head,
        );
        for s in set.open_stores() {
            s.collect_history(
                commit::generation_number(&s.snapshot().generation.name),
                s.head_commit().seq,
            );
        }
        self.quota.invalidate();
        tracing::info!(target: "sparkles::store::branching",
            branch = name,
            id = %e.id,
            force = o.force,
            retired = retire,
            "deleted a branch"
        );
        Ok(())
    }

    /// Rename branch `name` to `new`. The branch keeps its id, commits, storage and
    /// open store, and the branches created from it keep it as their upstream. Requests
    /// that name the old name answer `404 no-such-branch` afterwards.
    pub fn rename_branch(&self, name: &str, new: &str) -> Result<BranchInfo> {
        if name == MAIN {
            return Err(branch::invalid_branch("branch main cannot be renamed"));
        }
        branch::check_name(new)?;
        let set = self.owned_set()?.clone();
        let id = {
            let mut t = set.table.lock();
            if t.by_name(new).is_some() {
                return Err(branch::conflict(
                    "branch-exists",
                    format!("branch {new} exists"),
                ));
            }
            let mut next = t.clone();
            let e = next
                .branches
                .iter_mut()
                .find(|e| e.name == name)
                .ok_or_else(|| branch::no_such_branch(name))?;
            e.name = new.to_string();
            let id = e.id;
            // the commit point
            set.save(&next)?;
            *t = next;
            id
        };
        self.failpoint("branch-rename-committed");
        if let Some(s) = set.stores.lock().get(&id) {
            *s.branching.name.write() = new.to_string();
        }
        // the holds' names, as history listings show them
        set.refresh_holds(self);
        // the identity file follows; an open after a crash here writes it
        if !set.memory
            && let Err(e) = sync_branch_file(&set.branch_root(id), new)
        {
            tracing::warn!(target: "sparkles::store::branching",
                branch = new,
                "could not write the new name to {BRANCH_FILE}: {e}"
            );
        }
        tracing::info!(target: "sparkles::store::branching",
            from = name,
            to = new,
            id = %id,
            "renamed a branch"
        );
        self.branch_info(new)
    }

    /// After a rebuild of a linked branch: once its linked generation is gone from its
    /// history, the branch no longer reads the upstream, and its holds are released.
    pub(crate) fn add_relink_hold(&self, id: uuid::Uuid, segment: &Segment) -> Result<()> {
        let set = self.owned_set()?;
        {
            let mut table = set.table.lock();
            let mut next = table.clone();
            let entry = next
                .branches
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| branch::no_such_branch(&id.to_string()))?;
            let hold = GenHold {
                branch_id: segment.branch_id,
                generation: segment.generation.clone(),
            };
            if !entry.holds.contains(&hold) {
                entry.holds.push(hold);
            }
            set.save(&next)?;
            *table = next;
        }
        set.refresh_holds(self);
        Ok(())
    }

    /// Reconcile holds with every retained linked generation, including a relinked
    /// current generation. Failed publication can leave a conservative extra hold;
    /// opening/rebuilding the branch releases it after recovery has selected CURRENT.
    pub(crate) fn release_link_if_rebuilt(&self) {
        let Some(ident) = &self.branching.ident else {
            return;
        };
        let Some(set) = self.branching.set() else {
            return;
        };
        let mut holds = Vec::new();
        let dirs: Vec<_> = self
            .history
            .as_ref()
            .map(|history| {
                history
                    .lock()
                    .gens
                    .values()
                    .map(|g| g.dir.clone())
                    .collect()
            })
            .unwrap_or_default();
        for dir in dirs {
            let link = match link::read_link(&dir) {
                Ok(Some(link)) => link,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(target: "sparkles::store::branching", "could not reconcile branch link holds: {error}");
                    return; // unreadable metadata must never release a hold
                }
            };
            for segment in link.segments {
                let hold = GenHold {
                    branch_id: segment.branch_id,
                    generation: segment.generation,
                };
                if !holds.contains(&hold) {
                    holds.push(hold);
                }
            }
        }
        {
            let mut t = set.table.lock();
            let Some(i) = t.branches.iter().position(|e| e.id == self.dataset_id) else {
                return;
            };
            if t.branches[i].holds == holds {
                return;
            }
            let mut next = t.clone();
            next.branches[i].holds = holds;
            if let Err(e) = set.save(&next) {
                tracing::warn!(target: "sparkles::store::branching",
                    branch = self.branch_name(),
                    "could not release the branch's holds: {e}"
                );
                return;
            }
            *t = next;
        }
        tracing::info!(target: "sparkles::store::branching",
            branch = self.branch_name(),
            "reconciled the branch's upstream generation holds"
        );
        let _ = ident;
        // the upstream stores collect at their next collection point
        for s in set.open_stores() {
            s.install_branch_holds(&set);
        }
        set.unlinked.store(true, Ordering::Relaxed);
    }

    /// The merge bases of the heads of branches `a` and `b`.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<Vec<CommitRef>> {
        let set = self.owned_set()?;
        let ca = CommitRef {
            branch_id: self.branch_id_of(a)?,
            seq: self.branch(a)?.head_commit().seq,
        };
        let cb = CommitRef {
            branch_id: self.branch_id_of(b)?,
            seq: self.branch(b)?.head_commit().seq,
        };
        set.merge_bases(ca, cb)
    }

    /// The commits of branch `name`: its own, then those of the history it shares with
    /// its upstream, newest first (oldest first for [`CommitRange::After`]). Each with
    /// the branch that made it and, for a merge commit, the commit it merged. Commits
    /// whose metadata is gone are left out.
    pub fn branch_commits(
        &self,
        name: &str,
        range: CommitRange,
        limit: usize,
    ) -> Result<Vec<BranchCommit>> {
        let set = self.owned_set()?;
        let id = self.branch_id_of(name)?;
        let head = self.branch(name)?.head_commit().seq;
        let chain = set.chain(CommitRef {
            branch_id: id,
            seq: head,
        });
        // the seqs to list, with the branch whose commit each is
        let owner_of = |seq: u64| -> uuid::Uuid {
            chain
                .iter()
                .rev()
                .find(|(_, after, through)| seq <= *through && (seq > *after || *after == 0))
                .map_or(chain[0].0, |c| c.0)
        };
        let seqs: Box<dyn Iterator<Item = u64>> = match range {
            CommitRange::Latest => Box::new((0..=head).rev()),
            CommitRange::Before(b) => Box::new((0..b.min(head + 1)).rev()),
            CommitRange::After(a) => Box::new((a.saturating_add(1))..=head),
        };
        let mut stores: HashMap<uuid::Uuid, BranchStore<'_>> = HashMap::new();
        let mut out = Vec::new();
        let mut first_missing = 0usize;
        for seq in seqs {
            if out.len() >= limit {
                break;
            }
            let bid = owner_of(seq);
            if let std::collections::hash_map::Entry::Vacant(e) = stores.entry(bid) {
                e.insert(self.branch_by_id(bid)?);
            }
            let store = &stores[&bid];
            let Some(c) = store.commit(seq) else {
                // older commits than the catalog keeps: stop after a run of them
                first_missing += 1;
                if first_missing > 1024 {
                    break;
                }
                continue;
            };
            out.push(BranchCommit {
                commit: c,
                branch: set.name_of(bid),
                branch_id: bid,
                merged_from: store
                    .merge_record(seq)
                    .filter(|m| m.flags & MERGE_REPLAYED == 0)
                    .map(|m| set.named(m.source)),
                replayed_from: store
                    .merge_record(seq)
                    .filter(|m| m.flags & MERGE_REPLAYED != 0)
                    .map(|m| set.named(m.source)),
                annotation: store.annotation(seq),
            });
        }
        Ok(out)
    }

    /// The state of branch `name` at `at`, following the branch's history into its
    /// upstream for commits it shares with it.
    pub fn branch_snapshot_at(
        &self,
        name: &str,
        at: &At,
        o: &crate::history::HistoryOptions,
    ) -> Result<(Arc<Snapshot>, crate::history::Resolved)> {
        self.snapshot_following(self.branch(name)?, at, o)
    }

    /// [`branch_snapshot_at`](Self::branch_snapshot_at) from `store`: an inherited
    /// commit is read on the branch that made it, retired branches included.
    fn snapshot_following(
        &self,
        store: BranchStore<'_>,
        at: &At,
        o: &crate::history::HistoryOptions,
    ) -> Result<(Arc<Snapshot>, crate::history::Resolved)> {
        match store.snapshot_at(at, o) {
            Err(e) => match branch::inherited_commit(&e) {
                Some(c) => {
                    let owner = self.owned_set()?.normalize(c);
                    let up = self.branch_by_id(owner.branch_id)?;
                    drop(store);
                    match at {
                        // the upstream resolves a time before the branch started
                        At::Time(_) => self.snapshot_following(up, at, o),
                        _ => self.snapshot_following(up, &At::Commit(c.seq), o),
                    }
                }
                None => Err(e),
            },
            r => r,
        }
    }

    /// Resolve `at` on branch `name`, following its history into the upstream. Returns
    /// the commit and the branch id that made it.
    pub fn branch_resolve(
        &self,
        name: &str,
        at: &At,
    ) -> Result<(crate::history::Resolved, uuid::Uuid)> {
        self.resolve_following(self.branch(name)?, at)
    }

    fn resolve_following(
        &self,
        store: BranchStore<'_>,
        at: &At,
    ) -> Result<(crate::history::Resolved, uuid::Uuid)> {
        match store.resolve(at) {
            Err(e) => match branch::inherited_commit(&e) {
                Some(c) => {
                    let owner = self.owned_set()?.normalize(c);
                    let up = self.branch_by_id(owner.branch_id)?;
                    match at {
                        At::Time(_) => self.resolve_following(up, at),
                        _ => self.resolve_following(up, &At::Commit(c.seq)),
                    }
                }
                None => Err(e),
            },
            Ok(r) => Ok((r, store.dataset_id)),
        }
    }
}

/// A commit in a branch's history.
#[derive(Clone, Debug)]
pub struct BranchCommit {
    pub commit: CommitInfo,
    /// the branch that made it (`None` when it was deleted)
    pub branch: Option<String>,
    pub branch_id: uuid::Uuid,
    pub merged_from: Option<NamedCommitRef>,
    /// for a commit of a replayed fast-forward, the commit it replays
    pub replayed_from: Option<NamedCommitRef>,
    pub annotation: Option<crate::annotations::Annotation>,
}

/// Where a new branch starts.
pub(crate) struct Start {
    pub commit: CommitInfo,
    pub segments: Vec<Segment>,
    /// the state to build from, when the branch is built rather than linked
    pub snapshot: Option<Arc<Snapshot>>,
}

pub(crate) enum Captured {
    Here(Start),
    /// the commit belongs to an upstream branch
    Inherited(CommitRef),
}
