//! Recovery of independent LOADs before their names reach a materialization record.
use crate::dataset::RecordedReasoning;
use crate::sparql::pool::QueryPool;
use crate::{Result, store::Store};
#[cfg(feature = "reasoning")]
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, Serializer};
use sparkles_budget::MemoryLease;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

pub(crate) const FILE: &str = "reasoning-imports.json";

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
        let bytes = size_of::<Self>() + 4 * size_of::<usize>();
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
    fn snapshot(
        &self,
        publishing: &[Arc<PendingName>],
        parent: Option<sparkles_budget::ParentHandle>,
        limit: Option<u64>,
    ) -> Result<NameSnapshot> {
        let loaded = self.entries().count();
        let count = loaded + publishing.len();
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
        names.extend_from_slice(publishing);
        Ok(NameSnapshot {
            names,
            loaded,
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
        let snapshot = self.snapshot(&[], parent, options.max_memory_bytes)?;
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
        #[derive(Deserialize)]
        struct Saved {
            imports: Vec<String>,
            publishing: Vec<String>,
        }
        let mut pending = Self::default();
        let bytes = match std::fs::read(root.join(FILE)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(pending),
            Err(e) => return Err(e.into()),
        };
        let saved: Saved = crate::formats::settings_from_slice(FILE, &bytes)?;
        let mut reconcile = !saved.publishing.is_empty();
        for iri in saved.imports {
            if recorded.contains(&iri) || pending.contains(&iri) {
                reconcile = true;
                continue;
            }
            pending.push(Entry::prepare(&iri, pool)?);
        }
        // The pre-publication hook runs under the store writer. A crash can
        // leave the intent on either side of the commit of a newly populated graph.
        let snap = store.snapshot();
        for iri in saved.publishing {
            if let Some(id) = snap.lookup_iri(&iri)
                && snap.count(crate::index::Perm::Gspo, &[id.0])? != 0
                && !pending.contains(&iri)
                && !recorded.contains(&iri)
            {
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
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    loaded: usize,
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
        let (loaded, publishing) = self.names.split_at(self.loaded);
        let mut bytes = Vec::new();
        write_names(&mut bytes, loaded.iter().map(Arc::as_ref), publishing)?;
        Ok(Some(bytes))
    }
}

fn write_names<'a>(
    writer: impl Write,
    loaded: impl Iterator<Item = &'a PendingName> + Clone,
    publishing: &[Arc<PendingName>],
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
        format: 1,
        imports: Names(loaded),
        publishing: Names(publishing.iter().map(Arc::as_ref)),
    };
    serde_json::to_writer(writer, &saved).map_err(std::io::Error::from)
}

#[derive(Default)]
pub(crate) struct ImportJournal {
    pub(crate) loaded: PendingImports,
    publishing: Vec<Arc<PendingName>>,
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
    pub(crate) fn backup_snapshot(&self, pool: Option<&Arc<QueryPool>>) -> Result<NameSnapshot> {
        self.loaded
            .snapshot(&self.publishing, pool.map(|p| p.work_parent()), None)
    }

    fn save(&self, store: &Store) -> Result<()> {
        self.save_except(store, &[])
    }

    fn save_except(&self, store: &Store, recorded: &[String]) -> Result<()> {
        let Some(root) = store.root() else {
            return Ok(());
        };
        let path = root.join(FILE);
        let loaded = self
            .loaded
            .entries()
            .filter(|e| recorded.binary_search(&e.name.iri).is_err())
            .map(|entry| entry.name.as_ref());
        if self.publishing.is_empty() && loaded.clone().next().is_none() {
            return match std::fs::remove_file(path) {
                Ok(()) => super::sync_dir(root),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        Ok(crate::fsutil::write_atomic_with(&path, |file| {
            let mut writer = BufWriter::new(file);
            write_names(&mut writer, loaded, &self.publishing)?;
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
        let execute = |opts| crate::sparql::update::update(self.store(), update, opts).map(|_| ());
        let state = self.state();
        let known = {
            let journal = state.pending_imports.lock();
            journal.loaded.contains(iri)
                || state
                    .read_reasoning()
                    .as_ref()
                    .is_some_and(|r| r.fetched_imports.iter().any(|v| v == iri))
        };
        if known {
            return execute(opts);
        }
        let entry = Entry::prepare(iri, state.pool.as_ref())?;
        let mut options = opts.clone();
        let prior = options.write.before_publish.take();
        let dataset = self.clone();
        let intent = entry.name.clone();
        options.write.before_publish = Some(crate::guard::PublishHook::new(move || {
            if let Some(hook) = &prior {
                (hook.0)()?;
            }
            let mut journal = dataset.state().pending_imports.lock();
            if !journal
                .publishing
                .iter()
                .any(|entry| Arc::ptr_eq(entry, &intent))
            {
                journal.publishing.push(intent.clone());
                if let Err(error) = journal.save(dataset.store()) {
                    journal.publishing.pop();
                    journal.publishing.shrink_to_fit();
                    return Err(error);
                }
            }
            Ok(())
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(&options)));
        let settled = (|| -> Result<()> {
            let mut journal = state.pending_imports.lock();
            if let Some(index) = journal
                .publishing
                .iter()
                .position(|v| Arc::ptr_eq(v, &entry.name))
            {
                // A WAL fence or generation switch can fail after reaching disk.
                // Keep the admitted intent until reopen can see the recovered graph.
                if self.store().root().is_some()
                    && matches!(
                        &result,
                        Ok(Err(crate::Error::Poisoned | crate::Error::Io(_)))
                    )
                {
                    return Ok(());
                }
                journal.publishing.swap_remove(index);
                if journal.publishing.capacity() > journal.publishing.len().saturating_mul(4) {
                    journal.publishing.shrink_to_fit();
                }
                let loaded = match &result {
                    Ok(Ok(())) => true,
                    Ok(Err(_)) => false,
                    Err(_) => {
                        let snap = self.store().snapshot();
                        snap.lookup_iri(iri)
                            .map(|id| snap.count(crate::index::Perm::Gspo, &[id.0]))
                            .transpose()?
                            .unwrap_or(0)
                            != 0
                    }
                };
                if loaded {
                    if !journal.loaded.contains(iri) {
                        journal.record_load(entry);
                    }
                } else {
                    // Drop the refused intent from the durable list as well. A crash
                    // before this rewrite is reconciled against the recovered graph.
                    journal.save(self.store())?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(result) => {
                settled?;
                result
            }
            Err(panic) => {
                if let Err(error) = settled {
                    tracing::error!(%error, "could not reconcile an interrupted import");
                }
                std::panic::resume_unwind(panic)
            }
        }
    }
}

impl ImportJournal {
    pub(crate) fn clear_loaded(&mut self, store: &Store) -> Result<()> {
        self.clear_loaded_from(store, |_| true)
    }

    fn clear_loaded_from(&mut self, store: &Store, start: impl Fn(&Entry) -> bool) -> Result<()> {
        let mut tail = &mut self.loaded;
        while let Some(entry) = tail.0.as_ref() {
            if start(entry) {
                break;
            }
            tail = &mut tail.0.as_mut().expect("current link").older;
        }
        let Some(removed) = tail.0.take() else {
            return Ok(());
        };
        if let Err(error) = self.save(store) {
            let mut tail = &mut self.loaded;
            while let Some(ref mut entry) = tail.0 {
                tail = &mut entry.older;
            }
            tail.0 = Some(removed);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(feature = "reasoning")]
    fn record_load(&mut self, mut entry: Box<Entry>) {
        entry.loaded_after = self.next_run;
        self.loaded.push(entry);
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn finish_clear(&mut self, store: &Store, run: u64) -> Result<()> {
        // LOADs prepend under this lock. Retiring individual links between
        // preparation and publication preserves the order of the survivors.
        self.clear_loaded_from(store, |entry| entry.loaded_after < run)
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
            journal.record_load(Entry::prepare(iri, None).unwrap());
        }
        let first_clear = journal.prepare_run();
        journal.record_load(Entry::prepare("between", None).unwrap());
        let second_clear = journal.prepare_run();
        journal.record_load(Entry::prepare("new", None).unwrap());
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
