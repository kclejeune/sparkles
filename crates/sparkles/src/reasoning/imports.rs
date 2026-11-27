//! Recovery of independent LOADs before their names reach a materialization record.
use crate::Result;
use crate::dataset::RecordedReasoning;
use crate::sparql::pool::QueryPool;
use crate::store::{Store, import_journal};
#[cfg(feature = "reasoning")]
use parking_lot::Mutex;
use serde::{Serialize, Serializer};
use sparkles_budget::MemoryLease;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

pub(crate) use import_journal::FILE;

/// Membership is private to the dataset. Snapshots and branches share the names.
#[derive(Default)]
pub(crate) struct PendingImports(Option<Box<Entry>>);

struct Entry {
    // Local to this journal. Reopened and inherited links precede its first run.
    #[cfg(feature = "reasoning")]
    loaded_after: u64,
    name: Arc<PendingName>,
    older: PendingImports,
    _credit: Option<MemoryLease>,
}

struct PendingName {
    iri: String,
    #[cfg_attr(not(feature = "reasoning"), allow(dead_code))]
    account: Arc<NameAccount>,
    _overhead: Option<MemoryLease>,
}

/// One retained payload reservation follows the name until its last copy is
/// gone. Each overlapping copy has its own work lease.
struct NameAccount {
    _retained: Option<MemoryLease>,
    #[cfg(feature = "reasoning")]
    transfer: Mutex<Transfer>,
    _overhead: Option<MemoryLease>,
}

#[cfg(feature = "reasoning")]
#[derive(Default)]
struct Transfer {
    // The retained lease backs one live IRI. Every additional copy has an
    // equal payload charge, so releasing any copy can return the newest lease.
    overlap: Option<MemoryLease>,
    extra: Option<Box<ExtraCopy>>,
}

#[cfg(feature = "reasoning")]
struct ExtraCopy {
    older: Option<Box<ExtraCopy>>,
    credit: MemoryLease,
}

#[cfg(feature = "reasoning")]
impl NameAccount {
    fn add_copy(
        &self,
        bytes: usize,
        work: &mut sparkles_reasoner::PreparationMemory<'_>,
    ) -> Result<()> {
        let mut transfer = self.transfer.lock();
        let extra = transfer.overlap.is_some();
        let bytes = bytes + if extra { size_of::<ExtraCopy>() } else { 0 };
        let credit = work
            .reserve(bytes)
            .map_err(super::parent_error)?
            .expect("pooled import transfer");
        if extra {
            transfer.extra = Some(Box::new(ExtraCopy {
                older: transfer.extra.take(),
                credit,
            }));
        } else {
            transfer.overlap = Some(credit);
        }
        Ok(())
    }

    fn release_copy(&self) {
        let credit = {
            let mut transfer = self.transfer.lock();
            if let Some(extra) = transfer.extra.take() {
                let ExtraCopy { older, credit } = *extra;
                transfer.extra = older;
                Some(credit)
            } else {
                transfer.overlap.take()
            }
        };
        drop(credit);
    }
}

#[cfg(feature = "reasoning")]
impl Drop for PendingName {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.iri));
        self.account.release_copy();
    }
}

impl Serialize for PendingName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.iri)
    }
}

#[cfg(feature = "reasoning")]
pub(crate) struct NameClaim {
    pending: Arc<PendingName>,
    owner: RecordedImport,
}

#[cfg(feature = "reasoning")]
pub(crate) struct RecordedImport(Arc<NameAccount>);

#[cfg(feature = "reasoning")]
impl NameClaim {
    pub fn bytes(&self) -> u64 {
        self.pending.iri.len() as u64
    }

    pub fn is_exclusive(&self) -> bool {
        Arc::strong_count(&self.pending) == 1 && Arc::strong_count(&self.owner.0) == 2
    }

    pub fn into_exclusive(self) -> MemoryLease {
        let Self { pending, mut owner } = self;
        drop(pending);
        Arc::get_mut(&mut owner.0)
            .expect("private account")
            ._retained
            .take()
            .expect("pooled transfer")
    }

    pub fn publish(self) -> RecordedImport {
        self.owner
    }
}

#[cfg(feature = "reasoning")]
impl Drop for RecordedImport {
    fn drop(&mut self) {
        self.0.release_copy();
    }
}

impl Drop for PendingImports {
    fn drop(&mut self) {
        while let Some(mut entry) = self.0.take() {
            self.0 = entry.older.0.take();
        }
    }
}

impl Entry {
    fn prepare(iri: &str, pool: Option<&Arc<QueryPool>>) -> Result<Box<Self>> {
        let account_bytes = size_of::<NameAccount>() + 2 * size_of::<usize>();
        let name_bytes = size_of::<PendingName>() + 2 * size_of::<usize>();
        let mut credit =
            RecordedReasoning::reserve(pool, (iri.len() + account_bytes + name_bytes) as u64)?;
        let overhead = credit
            .as_mut()
            .map(|lease| lease.split(account_bytes as u64).expect("reserved account"));
        let payload = credit
            .as_mut()
            .map(|lease| lease.split(iri.len() as u64).expect("reserved IRI"));
        Self::link(
            Arc::new(PendingName {
                iri: iri.to_owned(),
                account: Arc::new(NameAccount {
                    _retained: payload,
                    #[cfg(feature = "reasoning")]
                    transfer: Mutex::default(),
                    _overhead: overhead,
                }),
                _overhead: credit,
            }),
            pool,
        )
    }

    fn link(name: Arc<PendingName>, pool: Option<&Arc<QueryPool>>) -> Result<Box<Self>> {
        let bytes = size_of::<Self>();
        Ok(Box::new(Self {
            #[cfg(feature = "reasoning")]
            loaded_after: 0,
            name,
            older: PendingImports::default(),
            _credit: RecordedReasoning::reserve(pool, bytes as u64)?,
        }))
    }
}

impl PendingImports {
    fn entries(&self) -> impl Iterator<Item = &Entry> + Clone {
        std::iter::successors(self.0.as_deref(), |entry| entry.older.0.as_deref())
    }

    pub(super) fn contains(&self, iri: &str) -> bool {
        self.entries().any(|e| e.name.iri == iri)
    }

    pub(crate) fn fork(&self, pool: Option<&Arc<QueryPool>>) -> Result<Self> {
        let mut list = Self::default();
        let mut tail = &mut list;
        for entry in self.entries() {
            tail = &mut tail.0.insert(Entry::link(entry.name.clone(), pool)?).older;
        }
        Ok(list)
    }

    #[cfg(any(feature = "reasoning", feature = "backup"))]
    fn snapshot<'a>(
        &self,
        publishing: impl Iterator<Item = &'a Publishing> + Clone,
        parent: Option<sparkles_budget::ParentHandle>,
        limit: Option<u64>,
    ) -> Result<NameSnapshot> {
        let count = self.entries().count() + publishing.clone().count();
        let bytes = (count * size_of::<Arc<PendingName>>()) as u64;
        if let Some(limit) = limit
            && bytes > limit
        {
            return Err(super::parent_error(sparkles_budget::ParentRefused::Limit {
                limit,
                requested: bytes,
                scope: sparkles_budget::Scope::Local,
            }));
        }
        let credit = parent
            .map(|p| p.reserve(bytes))
            .transpose()
            .map_err(super::parent_error)?;
        let mut names = Vec::with_capacity(count);
        names.extend(self.entries().map(|e| e.name.clone()));
        names.extend(publishing.map(|intent| intent.name.clone()));
        Ok(NameSnapshot {
            names,
            _credit: credit,
        })
    }

    #[cfg(feature = "reasoning")]
    pub(super) fn refresh_names(
        &self,
        options: &mut crate::sparql::QueryOptions,
    ) -> Result<NameSnapshot> {
        let parent = options
            .grant
            .as_ref()
            .map(|g| g.memory_parent())
            .or_else(|| options.pool.as_ref().map(|p| p.work_parent()));
        let snapshot = self.snapshot(std::iter::empty(), parent, options.max_memory_bytes)?;
        if let Some(limit) = &mut options.max_memory_bytes {
            *limit -= (snapshot.names.len() * size_of::<Arc<PendingName>>()) as u64;
        }
        Ok(snapshot)
    }

    #[cfg(feature = "reasoning")]
    pub(super) fn claim(
        &self,
        iri: &str,
        work: &mut sparkles_reasoner::PreparationMemory<'_>,
    ) -> Result<Option<NameClaim>> {
        let Some(entry) = self.entries().find(|e| e.name.iri == iri) else {
            return Ok(None);
        };
        entry.name.account.add_copy(iri.len(), work)?;
        Ok(Some(NameClaim {
            pending: entry.name.clone(),
            owner: RecordedImport(entry.name.account.clone()),
        }))
    }

    fn push(&mut self, mut entry: Box<Entry>) {
        entry.older.0 = self.0.take();
        self.0 = Some(entry);
    }

    pub(crate) fn read(
        root: &Path,
        store: &Store,
        pool: Option<&Arc<QueryPool>>,
        recorded: &[String],
    ) -> Result<Self> {
        let mut pending = Self::default();
        let Some(saved) = import_journal::read(root)? else {
            return Ok(pending);
        };
        let mut reconcile = !saved.publishing.is_empty();
        for iri in saved.imports {
            if recorded.contains(&iri) || pending.contains(&iri) {
                reconcile = true;
                continue;
            }
            pending.push(Entry::prepare(&iri, pool)?);
        }
        let snap = store.snapshot();
        for intent in saved.publishing {
            let (iri, committed) = match intent {
                import_journal::Intent::Legacy(iri) => {
                    // Older journals only recorded the name of the target graph.
                    let present = snap
                        .lookup_iri(&iri)
                        .map(|id| snap.count(crate::index::Perm::Gspo, &[id.0]))
                        .transpose()?
                        .is_some_and(|count| count != 0);
                    (iri, present)
                }
                import_journal::Intent::Tracked(p) => {
                    // The writer reserves this sequence for this LOAD. After it
                    // writes the intent, committing a different write at the
                    // same sequence requires reopen and this reconciliation.
                    (p.iri, p.commit <= snap.commit)
                }
            };
            if committed && !pending.contains(&iri) && !recorded.contains(&iri) {
                pending.push(Entry::prepare(&iri, pool)?);
            }
        }
        if reconcile {
            let journal = ImportJournal::from(pending);
            journal.save(store)?;
            return Ok(journal.loaded);
        }
        Ok(pending)
    }
}
#[derive(Default)]
#[cfg(any(feature = "reasoning", feature = "backup"))]
pub(crate) struct NameSnapshot {
    names: Vec<Arc<PendingName>>,
    _credit: Option<MemoryLease>,
}

#[cfg(feature = "reasoning")]
impl NameSnapshot {
    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|name| name.iri.as_str())
    }
}

#[cfg(feature = "backup")]
impl NameSnapshot {
    pub(crate) fn backup_bytes(&self) -> Result<Option<Vec<u8>>> {
        if self.names.is_empty() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        write_names(
            &mut bytes,
            self.names.iter().map(Arc::as_ref),
            std::iter::empty(),
        )?;
        Ok(Some(bytes))
    }
}

fn write_names<'a>(
    writer: impl Write,
    loaded: impl Iterator<Item = &'a PendingName> + Clone,
    publishing: impl Iterator<Item = &'a Publishing> + Clone,
) -> std::io::Result<()> {
    struct Names<I>(I);
    impl<I: Iterator + Clone> Serialize for Names<I>
    where
        I::Item: Serialize,
    {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            serializer.collect_seq(self.0.clone())
        }
    }
    #[derive(Serialize)]
    struct Saved<I: Serialize, P: Serialize> {
        format: u32,
        imports: I,
        publishing: P,
    }
    let saved = Saved {
        format: if publishing.clone().next().is_some() {
            2
        } else {
            1
        },
        imports: Names(loaded),
        publishing: Names(publishing.map(|p| import_journal::Publication {
            iri: p.name.as_ref(),
            commit: p.stamp().commit,
        })),
    };
    serde_json::to_writer(writer, &saved).map_err(std::io::Error::from)
}

struct Publishing {
    name: Arc<PendingName>,
    stamp: std::sync::OnceLock<PublicationStamp>,
    _credit: Option<MemoryLease>,
}

struct PublicationStamp {
    #[cfg(feature = "reasoning")]
    loaded_after: u64,
    commit: u64,
}

impl Publishing {
    fn stamp(&self) -> &PublicationStamp {
        self.stamp
            .get()
            .expect("intent stamped under the RDF writer")
    }

    #[cfg(feature = "reasoning")]
    fn new(name: Arc<PendingName>, pool: Option<&Arc<QueryPool>>) -> Result<Arc<Self>> {
        // Arc counters and the journal vector, which shrinks above four slots per intent.
        let bytes = size_of::<Self>() + 6 * size_of::<usize>();
        Ok(Arc::new(Self {
            name,
            stamp: Default::default(),
            _credit: RecordedReasoning::reserve(pool, bytes as u64)?,
        }))
    }
}

#[derive(Default)]
pub(crate) struct ImportJournal {
    pub(crate) loaded: PendingImports,
    publishing: Vec<Arc<Publishing>>,
    #[cfg(feature = "reasoning")]
    next_run: u64,
    /// Each run keeps a reference while its inputs can still change before preparation.
    #[cfg(feature = "reasoning")]
    pub(super) collecting: Arc<()>,
    #[cfg(feature = "reasoning")]
    pub(crate) published_run: u64,
}

impl From<PendingImports> for ImportJournal {
    fn from(loaded: PendingImports) -> Self {
        Self {
            loaded,
            ..Default::default()
        }
    }
}

impl ImportJournal {
    #[cfg(feature = "backup")]
    pub(crate) fn backup_snapshot(
        &self,
        pool: Option<&Arc<QueryPool>>,
        commit: u64,
    ) -> Result<NameSnapshot> {
        let published = self
            .publishing
            .iter()
            .map(Arc::as_ref)
            .filter(|p| p.stamp().commit <= commit);
        self.loaded
            .snapshot(published, pool.map(|p| p.work_parent()), None)
    }

    fn save(&self, store: &Store) -> Result<()> {
        self.save_except(store, &[])
    }

    fn save_except(&self, store: &Store, recorded: &[String]) -> Result<()> {
        self.save_selected(
            store,
            |e| recorded.binary_search(&e.name.iri).is_err(),
            |_| true,
        )
    }

    fn save_selected(
        &self,
        store: &Store,
        keep_loaded: impl Fn(&Entry) -> bool + Clone,
        keep_publishing: impl Fn(&Publishing) -> bool + Clone,
    ) -> Result<()> {
        let Some(root) = store.root() else {
            return Ok(());
        };
        let path = root.join(FILE);
        let loaded = self
            .loaded
            .entries()
            .filter(move |e| keep_loaded(e))
            .map(|entry| entry.name.as_ref());
        let publishing = self
            .publishing
            .iter()
            .map(Arc::as_ref)
            .filter(move |i| keep_publishing(i));
        if publishing.clone().next().is_none() && loaded.clone().next().is_none() {
            return match std::fs::remove_file(path) {
                Ok(()) => super::sync_dir(root),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        Ok(crate::fsutil::write_atomic_with(&path, |file| {
            let mut writer = BufWriter::new(file);
            write_names(&mut writer, loaded, publishing)?;
            writer.flush()
        })?)
    }
}

#[cfg(feature = "reasoning")]
impl crate::Dataset {
    pub(crate) fn load_reasoning_import(
        &self,
        iri: &str,
        update: &str,
        opts: &crate::sparql::QueryOptions,
    ) -> Result<()> {
        let state = self.state();
        // A replacement LOAD needs its own publication intent. Share an existing
        // IRI allocation, but reserve its link before the writer can commit.
        let pending_name = state
            .pending_imports
            .lock()
            .loaded
            .entries()
            .find(|e| e.name.iri == iri)
            .map(|e| e.name.clone());
        let entry = match pending_name {
            Some(name) => Entry::link(name, state.pool.as_ref())?,
            None => Entry::prepare(iri, state.pool.as_ref())?,
        };
        let intent = Publishing::new(entry.name.clone(), state.pool.as_ref())?;
        let persist = |commit| {
            let mut journal = state.pending_imports.lock();
            let stamp = PublicationStamp {
                loaded_after: journal.next_run,
                commit,
            };
            assert!(intent.stamp.set(stamp).is_ok(), "one commit per LOAD");
            journal.publishing.push(intent.clone());
            if let Err(error) = journal.save(self.store()) {
                journal.publishing.pop();
                journal.publishing.shrink_to_fit();
                return Err(error);
            }
            Ok(())
        };
        // A refusal after the intent's sync withdraws it from disk first. On failure
        // both copies stay, and the writer stays poisoned until reopen discards it.
        let retire = |_: u64| -> Result<()> {
            let mut journal = state.pending_imports.lock();
            let Some(index) = journal
                .publishing
                .iter()
                .position(|v| Arc::ptr_eq(v, &intent))
            else {
                return Ok(());
            };
            journal.save_selected(self.store(), |_| true, |p| !std::ptr::eq(p, &*intent))?;
            journal.publishing.swap_remove(index);
            Ok(())
        };
        let hooks = CommitHooks { persist, retire };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::sparql::update::update_with_commit_hook(self.store(), update, opts, &hooks)
                .map(|_| ())
        }));
        QueryPool::with_deferred_release_wakes(|| {
            let mut journal = state.pending_imports.lock();
            if let Some(index) = journal
                .publishing
                .iter()
                .position(|v| Arc::ptr_eq(v, &intent))
            {
                let stamp = intent.stamp();
                let committed = stamp.commit <= self.store().snapshot().commit;
                // A fence or generation switch can reach disk without reaching
                // the live snapshot. Keep that intent for recovery.
                if !committed && self.store().root().is_some() {
                    return;
                }
                journal.publishing.swap_remove(index);
                if journal.publishing.capacity() > journal.publishing.len().saturating_mul(4) {
                    journal.publishing.shrink_to_fit();
                }
                if committed {
                    journal.record_load(entry, stamp.loaded_after);
                }
            }
        });
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

#[cfg(feature = "reasoning")]
struct CommitHooks<P, R> {
    persist: P,
    retire: R,
}

#[cfg(feature = "reasoning")]
impl<P, R> crate::guard::CommitIntent for CommitHooks<P, R>
where
    P: Fn(u64) -> Result<()> + Sync,
    R: Fn(u64) -> Result<()> + Sync,
{
    fn persist(&self, seq: u64) -> Result<()> {
        (self.persist)(seq)
    }
    fn retire(&self, seq: u64) -> Result<()> {
        (self.retire)(seq)
    }
}

impl ImportJournal {
    pub(crate) fn clear_loaded(&mut self, store: &Store) -> Result<()> {
        self.clear_before(store, |_| false, |_| true)
    }

    fn clear_before(
        &mut self,
        store: &Store,
        keep_loaded: impl Fn(&Entry) -> bool + Copy,
        keep_publishing: impl Fn(&Publishing) -> bool + Copy,
    ) -> Result<()> {
        if self.loaded.entries().all(keep_loaded)
            && self.publishing.iter().all(|i| keep_publishing(i))
        {
            return Ok(());
        }
        // Write the selected membership before releasing any links. On failure
        // both lists still match the previous journal and can be retried.
        self.save_selected(store, keep_loaded, keep_publishing)?;
        let mut tail = &mut self.loaded;
        while tail.0.as_deref().is_some_and(keep_loaded) {
            tail = &mut tail.0.as_mut().expect("current link").older;
        }
        tail.0 = None;
        self.publishing.retain(|i| keep_publishing(i));
        if self.publishing.capacity() > self.publishing.len().saturating_mul(4) {
            self.publishing.shrink_to_fit();
        }
        Ok(())
    }

    #[cfg(feature = "reasoning")]
    fn record_load(&mut self, mut entry: Box<Entry>, loaded_after: u64) {
        // Writers stamp their intents in order, but can settle out of order.
        let mut link = &mut self.loaded;
        while let Some(existing) = link.0.as_ref() {
            if existing.name.iri == entry.name.iri {
                if existing.loaded_after >= loaded_after {
                    return;
                }
                let mut existing = link.0.take().expect("current link");
                link.0 = existing.older.0.take();
                entry = existing;
                break;
            }
            link = &mut link.0.as_mut().expect("current link").older;
        }
        entry.loaded_after = loaded_after;
        let mut link = &mut self.loaded;
        while link
            .0
            .as_ref()
            .is_some_and(|e| e.loaded_after > loaded_after)
        {
            link = &mut link.0.as_mut().expect("current link").older;
        }
        link.push(entry);
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn finish_clear(&mut self, store: &Store, run: u64) -> Result<()> {
        // A committed writer can still be settling its intent. Retire that
        // intent at the same boundary so it cannot revive an older load.
        self.clear_before(
            store,
            |e| e.loaded_after >= run,
            |i| i.stamp().loaded_after >= run,
        )
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn prepare_run(&mut self) -> u64 {
        self.next_run = self
            .next_run
            .checked_add(1)
            .expect("materialization sequence exhausted");
        self.next_run
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn is_latest(&self, run: u64) -> bool {
        run == self.next_run
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn can_retire(&self, run: u64) -> bool {
        // A later writer or a run still collecting inputs can publish a record
        // that omits these names. Leave its recovery source in the journal.
        self.is_latest(run) && Arc::strong_count(&self.collecting) == 1
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn retire(&mut self, store: &Store, recorded: &[String]) -> Result<()> {
        if recorded.is_empty()
            || !self
                .loaded
                .entries()
                .any(|e| recorded.binary_search(&e.name.iri).is_ok())
        {
            return Ok(());
        }
        // The durable rewrite and the in-place unlink select the same names
        // under the journal lock. A write failure leaves membership intact.
        self.save_except(store, recorded)?;
        let mut cursor = &mut self.loaded;
        while let Some(entry) = cursor.0.as_ref() {
            if recorded.binary_search(&entry.name.iri).is_ok() {
                let mut removed = cursor.0.take().expect("current link");
                cursor.0 = removed.older.0.take();
            } else {
                cursor = &mut cursor.0.as_mut().expect("current link").older;
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "reasoning"))]
mod tests {
    use super::*;

    #[test]
    fn delayed_clear_removes_only_the_journal_entries_it_observed() {
        let dir = tempfile::tempdir().unwrap();
        let ds = crate::Dataset::open(dir.path()).unwrap();
        let mut journal = ImportJournal::default();
        for iri in ["old", "also-old"] {
            journal.record_load(Entry::prepare(iri, None).unwrap(), journal.next_run);
        }
        let first_clear = journal.prepare_run();
        journal.record_load(Entry::prepare("between", None).unwrap(), journal.next_run);
        let second_clear = journal.prepare_run();
        journal.record_load(Entry::prepare("new", None).unwrap(), journal.next_run);
        journal.finish_clear(ds.store(), first_clear).unwrap();
        assert!(journal.loaded.contains("between"));
        assert!(!journal.loaded.contains("old"));
        journal.finish_clear(ds.store(), second_clear).unwrap();
        let saved = PendingImports::read(dir.path(), ds.store(), None, &[]).unwrap();
        for pending in [&journal.loaded, &saved] {
            assert_eq!(pending.entries().count(), 1);
            assert!(pending.contains("new"));
        }
    }

    #[test]
    fn clear_orders_unsettled_intents_and_out_of_order_load_completions() {
        let dir = tempfile::tempdir().unwrap();
        let ds = crate::Dataset::open(dir.path()).unwrap();
        let mut journal = ImportJournal::default();
        let clear = journal.prepare_run();
        let newer = journal.prepare_run();
        for (iri, stamp) in [("new", newer), ("old", 0), ("middle", clear), ("new", 0)] {
            journal.record_load(Entry::prepare(iri, None).unwrap(), stamp);
        }
        for (iri, stamp) in [("before", 0), ("after", clear)] {
            let intent = Publishing::new(Entry::prepare(iri, None).unwrap().name, None).unwrap();
            assert!(
                intent
                    .stamp
                    .set(PublicationStamp {
                        loaded_after: stamp,
                        commit: stamp + 1
                    })
                    .is_ok()
            );
            journal.publishing.push(intent);
        }
        // A failed write must leave both lists available to the retry.
        std::fs::create_dir(dir.path().join("reasoning-imports.tmp")).unwrap();
        assert!(journal.finish_clear(ds.store(), clear).is_err());
        assert!(journal.loaded.contains("old"));
        assert_eq!(journal.publishing.len(), 2);
        std::fs::remove_dir(dir.path().join("reasoning-imports.tmp")).unwrap();
        journal.finish_clear(ds.store(), clear).unwrap();
        assert_eq!(journal.publishing.len(), 1);
        assert_eq!(journal.publishing[0].name.iri, "after");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(saved["imports"], serde_json::json!(["new", "middle"]));
        assert_eq!(saved["format"], 2);
        assert_eq!(
            saved["publishing"],
            serde_json::json!([{ "iri": "after", "commit": clear + 1 }])
        );
    }

    #[test]
    fn retirement_preserves_new_entries_and_unrecorded_members_of_the_snapshot() {
        let ds = crate::Dataset::memory();
        let mut journal = ImportJournal::default();
        for iri in ["old", "covered", "other", "also-covered"] {
            journal.loaded.push(Entry::prepare(iri, None).unwrap());
        }
        let snapshot = journal.loaded.fork(None).unwrap();
        journal.loaded.push(Entry::prepare("new", None).unwrap());
        journal
            .retire(ds.store(), &["also-covered".into(), "covered".into()])
            .unwrap();
        let names: Vec<_> = journal
            .loaded
            .entries()
            .map(|entry| entry.name.iri.as_str())
            .collect();
        assert_eq!(names, ["new", "other", "old"]);
        assert!(snapshot.contains("covered"));
    }

    #[test]
    fn transfer_scratch_shares_the_materialization_limit() {
        use crate::reasoning::{ReasonRequest, reasoner::Profile};
        let iri = format!("urn:{}", "x".repeat(256 << 10));
        let request = ReasonRequest {
            profile: Profile::RdfsSimple,
            incremental: false,
            max_memory: Some(iri.len() + 1),
            ..Default::default()
        };
        for retained_capacity in [4 << 20, 384 << 10] {
            let pool = QueryPool::with_retained_capacity(16 << 20, retained_capacity).unwrap();
            let ds = crate::Dataset::from_store_with(
                Store::in_memory(Default::default()),
                crate::DatasetOptions {
                    pool: Some(pool.clone()),
                    closure_cache_triples: 0,
                    ..Default::default()
                },
            );
            ds.update(&format!("INSERT DATA {{ <urn:o> <http://www.w3.org/2002/07/owl#imports> <{iri}> . GRAPH <{iri}> {{ <urn:s> <urn:p> 1 }} }}")).unwrap();
            ds.state()
                .pending_imports
                .lock()
                .loaded
                .push(Entry::prepare(&iri, Some(&pool)).unwrap());
            if retained_capacity == 4 << 20 {
                ds.reasoning().run(&request).unwrap();
            } else {
                let retained = pool.domain_stats().retained_reserved;
                let head = ds.store().snapshot().commit;
                let err = ds.reasoning().run(&request).unwrap_err();
                assert!(
                    matches!(err, crate::Error::BudgetExceeded(crate::Budget { kind: crate::BudgetKind::Memory, limit, requested }) if limit == request.max_memory.unwrap() as u64 && requested > limit),
                    "{err:?}"
                );
                assert_eq!(ds.store().snapshot().commit, head);
                assert!(ds.reasoning().record().is_none());
                assert_eq!(pool.domain_stats().retained_reserved, retained);
                let retry = ReasonRequest {
                    max_memory: Some(2 << 20),
                    ..request.clone()
                };
                let result = ds.reasoning().run(&retry).unwrap();
                assert!(result.report.peak_bytes > request.max_memory.unwrap() as u64);
            }
            assert_eq!(pool.domain_stats().work_reserved, 0);
        }
    }

    #[test]
    fn an_import_reader_keeps_only_the_working_copy_after_record_publication() {
        let pool = QueryPool::with_retained_capacity(16 << 20, 24 << 10).unwrap();
        let iri = format!("urn:{}", "x".repeat(16 << 10));
        let ds = crate::Dataset::from_store_with(
            Store::in_memory(Default::default()),
            crate::DatasetOptions {
                pool: Some(pool.clone()),
                closure_cache_triples: 0,
                ..Default::default()
            },
        );
        ds.update(&format!("INSERT DATA {{ <urn:o> <http://www.w3.org/2002/07/owl#imports> <{iri}> . GRAPH <{iri}> {{ <urn:s> <urn:p> 1 }} }}")).unwrap();
        let reader = {
            let mut journal = ds.state().pending_imports.lock();
            journal
                .loaded
                .push(Entry::prepare(&iri, Some(&pool)).unwrap());
            journal.loaded.0.as_ref().unwrap().name.clone()
        };
        ds.reasoning().run(&Default::default()).unwrap();
        assert_eq!(reader.iri, iri);
        assert_eq!(pool.domain_stats().work_reserved, iri.len() as u64);
        ds.reasoning().clear().unwrap();
        drop(reader);
        assert_eq!(pool.domain_stats().work_reserved, 0);
        assert_eq!(pool.domain_stats().retained_reserved, 0);
    }
}
