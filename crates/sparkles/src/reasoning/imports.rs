//! Recovery of independent LOADs before their names reach a materialization record.
use crate::dataset::{DatasetState, RecordedReasoning};
use crate::sparql::pool::QueryPool;
use crate::{Result, store::Store};
use serde::{Deserialize, Serialize, Serializer};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, OnceLock};

pub(crate) const FILE: &str = "reasoning-imports.json";

/// Branches and materializations can borrow one immutable list and its reservations.
#[derive(Clone, Default)]
pub(crate) struct PendingImports(Option<Arc<Entry>>);

struct Entry {
    iri: String,
    older: OnceLock<PendingImports>,
    _credit: Option<sparkles_budget::MemoryLease>,
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
        // Include the Arc header and a publishing slot (Vec's first allocation is four).
        let bytes = iri
            .len()
            .saturating_add(size_of::<Self>() + 6 * size_of::<usize>());
        let credit = RecordedReasoning::reserve(pool, bytes as u64)?;
        Ok(Arc::new(Self {
            iri: iri.to_owned(),
            older: OnceLock::new(),
            _credit: credit,
        }))
    }
}

impl PendingImports {
    pub(crate) fn names(&self) -> impl Iterator<Item = &String> + Clone {
        std::iter::successors(self.0.as_deref(), |entry| entry.older.get()?.0.as_deref())
            .map(|entry| &entry.iri)
    }

    fn push(&mut self, entry: Arc<Entry>) {
        assert!(
            entry.older.set(self.clone()).is_ok(),
            "an import publishes once"
        );
        self.0 = Some(entry);
    }

    pub(crate) fn read(root: &Path, store: &Store, pool: Option<&Arc<QueryPool>>) -> Result<Self> {
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
        for iri in saved.imports {
            pending.push(Entry::prepare(&iri, pool)?);
        }
        // The pre-publication hook runs under the store writer. A crash can
        // leave the intent on either side of the commit of a newly populated graph.
        let snap = store.snapshot();
        let reconcile = !saved.publishing.is_empty();
        for iri in saved.publishing {
            if let Some(id) = snap.lookup_iri(&iri)
                && snap.count(crate::index::Perm::Gspo, &[id.0])? != 0
                && !pending.names().any(|known| known == &iri)
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
            imports: Names(self.names()),
            publishing: Names(publishing.iter().map(|entry| &entry.iri)),
        };
        serde_json::to_writer(writer, &saved).map_err(std::io::Error::from)
    }

    #[cfg(feature = "backup")]
    pub(crate) fn backup_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.write(&mut bytes, &[])?;
        Ok(bytes)
    }
}

#[derive(Default)]
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
        if state
            .read_reasoning()
            .as_ref()
            .is_some_and(|r| r.fetched_imports.iter().any(|v| v == iri))
            || state
                .pending_imports
                .lock()
                .loaded
                .names()
                .any(|v| v == iri)
        {
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
                    if !journal.loaded.names().any(|v| v == iri) {
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

impl DatasetState {
    pub(crate) fn forget_recorded_imports(&self, recorded: &PendingImports) -> Result<()> {
        let mut journal = self.pending_imports.lock();
        if let (Some(a), Some(b)) = (&journal.loaded.0, &recorded.0)
            && Arc::ptr_eq(a, b)
            && journal.publishing.is_empty()
        {
            // The caller still owns the retired names and their credit.
            let previous = std::mem::take(&mut journal.loaded);
            if let Err(error) = journal.save(&self.store) {
                journal.loaded = previous;
                return Err(error);
            }
        }
        Ok(())
    }
}
