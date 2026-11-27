//! Recovery of independent LOADs before their names reach a materialization record.
use crate::dataset::RecordedReasoning;
use crate::sparql::pool::QueryPool;
use crate::{Result, store::Store};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, Serializer};
use sparkles_budget::MemoryLease;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, OnceLock};

pub(crate) const FILE: &str = "reasoning-imports.json";

/// Branches and materializations can borrow one immutable list and its reservations.
#[derive(Clone, Default)]
pub(crate) struct PendingImports(Option<Arc<Entry>>);

struct Entry {
    name: Arc<ImportName>,
    older: OnceLock<PendingImports>,
    _credit: Option<MemoryLease>,
}

struct ImportName {
    owner: Mutex<NameOwner>,
    _overhead: Option<MemoryLease>,
}

#[derive(Clone)]
pub(super) enum NameOwner {
    Pending(Arc<PendingName>),
    #[cfg(feature = "reasoning")]
    Recorded(Arc<RecordedReasoning>, usize),
}

pub(super) struct PendingName {
    iri: String,
    _memory: Mutex<Option<MemoryLease>>,
    #[cfg(feature = "reasoning")]
    claimed: std::sync::atomic::AtomicBool,
    _overhead: Option<MemoryLease>,
}

impl AsRef<str> for NameOwner {
    fn as_ref(&self) -> &str {
        match self {
            Self::Pending(name) => &name.iri,
            #[cfg(feature = "reasoning")]
            Self::Recorded(record, index) => {
                &record
                    .value
                    .as_ref()
                    .expect("published record")
                    .fetched_imports[*index]
            }
        }
    }
}

impl Serialize for ImportName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.owner.lock().as_ref())
    }
}

/// A provisional name stays readable across commit and rollback. On publication
/// its work lease covers any outstanding fetch reader, and its retained lease
/// moves to the record. Journal aliases then read the name from that record.
#[cfg(feature = "reasoning")]
pub(crate) struct NameClaim {
    slot: Arc<ImportName>,
    pending: Arc<PendingName>,
    work: Option<MemoryLease>,
    index: usize,
}

#[cfg(feature = "reasoning")]
impl NameClaim {
    pub fn bytes(&self) -> u64 {
        self.pending.iri.len() as u64
    }

    pub fn locate(&mut self, names: &[String]) {
        self.index = names
            .binary_search(&self.pending.iri)
            .expect("prepared import");
    }

    pub fn transfer(&mut self) -> MemoryLease {
        std::mem::replace(&mut *self.pending._memory.lock(), self.work.take())
            .expect("claimed retained name")
    }

    pub fn publish(self, record: &Arc<RecordedReasoning>) {
        *self.slot.owner.lock() = NameOwner::Recorded(record.clone(), self.index);
    }
}

#[cfg(feature = "reasoning")]
impl Drop for NameClaim {
    fn drop(&mut self) {
        self.pending
            .claimed
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for PendingImports {
    fn drop(&mut self) {
        // Successive failed runs can leave more names than one discovery pass.
        let mut head = self.0.take();
        while let Some(mut entry) = head.and_then(Arc::into_inner) {
            head = entry.older.take().and_then(|mut older| older.0.take());
        }
    }
}

impl Entry {
    fn prepare(iri: &str, pool: Option<&Arc<QueryPool>>) -> Result<Arc<Self>> {
        let overhead = size_of::<ImportName>() + size_of::<PendingName>() + 4 * size_of::<usize>();
        let mut credit = RecordedReasoning::reserve(pool, (iri.len() + overhead) as u64)?;
        let text_overhead = credit.as_mut().map(|l| {
            l.split((size_of::<PendingName>() + 2 * size_of::<usize>()) as u64)
                .expect("reserved name owner")
        });
        let payload = credit
            .as_mut()
            .map(|l| l.split(iri.len() as u64).expect("reserved IRI"));
        Self::link(
            Arc::new(ImportName {
                owner: Mutex::new(NameOwner::Pending(Arc::new(PendingName {
                    iri: iri.to_owned(),
                    _memory: Mutex::new(payload),
                    #[cfg(feature = "reasoning")]
                    claimed: false.into(),
                    _overhead: text_overhead,
                }))),
                _overhead: credit,
            }),
            pool,
        )
    }

    fn link(name: Arc<ImportName>, pool: Option<&Arc<QueryPool>>) -> Result<Arc<Self>> {
        let bytes = size_of::<Self>() + 6 * size_of::<usize>();
        let credit = RecordedReasoning::reserve(pool, bytes as u64)?;
        Ok(Arc::new(Self {
            name,
            older: OnceLock::new(),
            _credit: credit,
        }))
    }
}

impl PendingImports {
    fn entries(&self) -> impl Iterator<Item = &Arc<Entry>> + Clone {
        std::iter::successors(self.0.as_ref(), |entry| entry.older.get()?.0.as_ref())
    }

    pub(super) fn contains(&self, iri: &str) -> bool {
        self.entries().any(|e| e.name.owner.lock().as_ref() == iri)
    }

    #[cfg(feature = "reasoning")]
    pub(super) fn names(&self) -> impl Iterator<Item = NameOwner> {
        self.entries().map(|e| e.name.owner.lock().clone())
    }

    #[cfg(feature = "reasoning")]
    pub(super) fn claim(&self, iri: &str, pool: &Arc<QueryPool>) -> Result<Option<NameClaim>> {
        use std::sync::atomic::Ordering;
        for e in self.entries() {
            let owner = e.name.owner.lock();
            if let NameOwner::Pending(pending) = &*owner
                && pending.iri == iri
                && !pending.claimed.load(Ordering::Acquire)
            {
                let work = pool
                    .work_parent()
                    .reserve(iri.len() as u64)
                    .map_err(super::parent_error)?;
                pending.claimed.store(true, Ordering::Release);
                return Ok(Some(NameClaim {
                    slot: e.name.clone(),
                    pending: pending.clone(),
                    work: Some(work),
                    index: 0,
                }));
            }
        }
        Ok(None)
    }

    fn push(&mut self, entry: Arc<Entry>) {
        assert!(
            entry.older.set(self.clone()).is_ok(),
            "an import publishes once"
        );
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
            ImportJournal::from(pending.clone()).save(store)?;
        }
        Ok(pending)
    }

    fn write(&self, writer: impl Write, publishing: &[Arc<Entry>]) -> std::io::Result<()> {
        struct Names<I>(I);
        impl<I: Iterator + Clone> Serialize for Names<I>
        where
            I::Item: Serialize,
        {
            fn serialize<S: Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
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
            imports: Names(self.entries().map(|entry| entry.name.as_ref())),
            publishing: Names(publishing.iter().map(|entry| entry.name.as_ref())),
        };
        serde_json::to_writer(writer, &saved).map_err(std::io::Error::from)
    }
}

#[derive(Clone, Default)]
pub(crate) struct ImportJournal {
    pub(crate) loaded: PendingImports,
    publishing: Vec<Arc<Entry>>,
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
    pub(crate) fn backup_bytes(&self) -> Result<Option<Vec<u8>>> {
        if self.loaded.0.is_none() && self.publishing.is_empty() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        self.loaded.write(&mut bytes, &self.publishing)?;
        Ok(Some(bytes))
    }

    fn save(&self, store: &Store) -> Result<()> {
        let Some(root) = store.root() else {
            return Ok(());
        };
        let path = root.join(FILE);
        if self.loaded.0.is_none() && self.publishing.is_empty() {
            return match std::fs::remove_file(path) {
                Ok(()) => super::sync_dir(root),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        Ok(crate::fsutil::write_atomic_with(&path, |file| {
            let mut writer = BufWriter::new(file);
            self.loaded.write(&mut writer, &self.publishing)?;
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
        let intent = entry.clone();
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
                .position(|v| Arc::ptr_eq(v, &entry))
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
                        journal.loaded.push(entry);
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
    fn replace_loaded(&mut self, store: &Store, next: PendingImports) -> Result<()> {
        let previous = std::mem::replace(&mut self.loaded, next);
        if let Err(error) = self.save(store) {
            self.loaded = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn clear_loaded(&mut self, store: &Store) -> Result<()> {
        if self.loaded.0.is_some() {
            self.replace_loaded(store, PendingImports::default())?;
        }
        Ok(())
    }

    #[cfg(feature = "reasoning")]
    pub(crate) fn retire(
        &mut self,
        store: &Store,
        pool: Option<&Arc<QueryPool>>,
        plan: Option<Retirement>,
    ) -> Result<()> {
        let Some(Retirement { before, mut after }) = plan else {
            return Ok(());
        };
        let Some(new_count) = self
            .loaded
            .entries()
            .position(|node| Arc::ptr_eq(node, &before))
        else {
            // Another publication already changed the captured list.
            return Ok(());
        };
        if new_count != 0 {
            // Concurrent LOADs prepended new names after this run prepared its
            // record. Keep all of them ahead of the prepared remainder.
            after =
                match PendingImports::copy_onto(self.loaded.entries().take(new_count), after, pool)
                {
                    Ok(list) => list,
                    // The previous journal remains valid. A later run or reopen
                    // removes the redundant record entries without losing a LOAD.
                    Err(crate::Error::BudgetExceeded(_)) => return Ok(()),
                    Err(error) => return Err(error),
                };
        }
        self.replace_loaded(store, after)
    }
}

/// Immutable membership and its prepared replacement. Ordinary cleanup needs
/// no further admission after the inference commit.
#[cfg(feature = "reasoning")]
pub(crate) struct Retirement {
    before: Arc<Entry>,
    after: PendingImports,
}

#[cfg(feature = "reasoning")]
impl PendingImports {
    fn copy_onto<'a>(
        entries: impl Iterator<Item = &'a Arc<Entry>>,
        mut tail: Self,
        pool: Option<&Arc<QueryPool>>,
    ) -> Result<Self> {
        let mut reversed = Self::default();
        for entry in entries {
            reversed.push(Entry::link(entry.name.clone(), pool)?);
        }
        while let Some(mut head) = reversed.0.take() {
            let private = Arc::get_mut(&mut head).expect("unpublished journal link");
            reversed = private.older.take().unwrap_or_default();
            tail.push(head);
        }
        Ok(tail)
    }

    pub(super) fn retirement(
        &self,
        pool: Option<&Arc<QueryPool>>,
        recorded: &[String],
    ) -> Result<Option<Retirement>> {
        let Some(before) = self.0.as_ref().filter(|_| !recorded.is_empty()) else {
            return Ok(None);
        };
        let covered = |node: &Entry| {
            let name = node.name.owner.lock();
            recorded
                .binary_search_by(|iri| iri.as_str().cmp(name.as_ref()))
                .is_ok()
        };
        let Some((index, last)) = self
            .entries()
            .enumerate()
            .filter(|(_, e)| covered(e))
            .last()
        else {
            return Ok(None);
        };
        let tail = last.older.get().cloned().unwrap_or_default();
        let after = Self::copy_onto(
            self.entries().take(index).filter(|node| !covered(node)),
            tail,
            pool,
        )?;
        Ok(Some(Retirement {
            before: before.clone(),
            after,
        }))
    }
}

#[cfg(all(test, feature = "reasoning"))]
mod tests {
    use super::*;

    #[test]
    fn retirement_preserves_new_entries_and_unrecorded_members_of_the_snapshot() {
        let ds = crate::Dataset::memory();
        let mut journal = ImportJournal::default();
        for iri in ["old", "covered", "other", "also-covered"] {
            journal.loaded.push(Entry::prepare(iri, None).unwrap());
        }
        let snapshot = journal.loaded.clone();
        let plan = snapshot
            .retirement(None, &["also-covered".into(), "covered".into()])
            .unwrap();
        journal.loaded.push(Entry::prepare("new", None).unwrap());
        journal.retire(ds.store(), None, plan).unwrap();
        let names: Vec<_> = journal
            .loaded
            .names()
            .map(|name| name.as_ref().to_owned())
            .collect();
        assert_eq!(names, ["new", "other", "old"]);
        assert!(snapshot.contains("covered"));
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
            journal.loaded.names().next().unwrap()
        };
        ds.reasoning().run(&Default::default()).unwrap();
        assert_eq!(reader.as_ref(), iri);
        assert_eq!(pool.domain_stats().work_reserved, iri.len() as u64);
        ds.reasoning().clear().unwrap();
        drop(reader);
        assert_eq!(pool.domain_stats().work_reserved, 0);
        assert_eq!(pool.domain_stats().retained_reserved, 0);
    }
}
