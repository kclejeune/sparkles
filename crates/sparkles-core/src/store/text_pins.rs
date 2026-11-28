//! Retained full-text indexes of named RDF snapshots. The RDF pin owns the directory.
use super::*;

const ROOT: &str = "history-text";
#[cfg(feature = "text")]
const META: &str = "text.snapshot.json";
#[cfg(feature = "text")]
const MAX_META_BYTES: u64 = managed_history::MAX_BYTES as u64;

#[cfg(feature = "text")]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TextPin {
    seq: u64,
    config: crate::text::TextConfig,
}

impl Store {
    /// Clean staging made before a crash and restore the disk charge of live pins.
    pub(super) fn recover_text_pins(&self) {
        let (Some(root), Some(history)) = (&self.root, &self.history) else {
            return;
        };
        let entries = match std::fs::read_dir(root.join(ROOT)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(target: "sparkles::store", %error, "could not list snapshot text directories");
                return;
            }
        };
        let mut history = history.lock();
        let mut indexes: BTreeMap<_, _> = history
            .pins
            .values_mut()
            .filter_map(|pin| pin.text_index.map(|id| (id, pin)))
            .collect();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(target: "sparkles::store", %error, "could not inspect a snapshot text directory");
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {}
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(target: "sparkles::store", %error, %name, "could not stat a snapshot text directory");
                    continue;
                }
            }
            let id = uuid::Uuid::parse_str(name).ok();
            if let Some(pin) = id.as_ref().and_then(|id| indexes.get_mut(id)) {
                pin.text_bytes = dir_size(&entry.path());
            } else if (id.is_some() || name.starts_with(".text-pin-"))
                && let Err(error) = remove_dir(&entry.path())
            {
                tracing::warn!(target: "sparkles::store", %error, %name, "could not collect a snapshot text directory");
            }
        }
    }

    pub(super) fn remove_text_pin(&self, id: Option<uuid::Uuid>) {
        if let (Some(root), Some(id)) = (&self.root, id) {
            if let Err(error) = remove_dir(&root.join(ROOT).join(id.to_string())) {
                tracing::warn!(target: "sparkles::store", %error, %id, "could not collect a snapshot text index");
            }
            self.quota.invalidate();
        }
    }

    pub(super) fn text_pin_writer(
        &self,
        controls: &crate::history::HistoryOptions,
    ) -> Result<MutexGuard<'_, WriterState>> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let options = crate::guard::WriteOptions {
            cancel: controls.cancel.clone(),
            deadline: controls.deadline,
            ..Default::default()
        };
        let limit = crate::history::ReadLimit {
            options: controls,
            memory: None,
            held: 0,
        };
        let mut writer = limit.lock(&self.writer)?;
        self.drain_group(&mut writer, Some(&options))?;
        if writer.closed {
            return Err(Error::Cancelled);
        }
        if writer.poisoned {
            return Err(Error::Poisoned);
        }
        Ok(writer)
    }

    /// Rebuild or add an index without changing the RDF pin.
    pub fn rebuild_snapshot_text(
        &self,
        name: &str,
        controls: &crate::history::HistoryOptions,
    ) -> Result<crate::history::NamedSnapshot> {
        #[cfg(not(feature = "text"))]
        {
            let _ = (name, controls);
            Err(crate::text::not_built())
        }
        #[cfg(feature = "text")]
        {
            let (Some(root), Some(history)) = (&self.root, &self.history) else {
                return Err(Error::HistoryUnsupported(
                    "a snapshot text index needs a persistent dataset".into(),
                ));
            };
            let w = self.text_pin_writer(controls)?;
            let at = crate::history::At::Snapshot(name.to_owned());
            let resolved = self.resolve_with(&at, w.head)?;
            let limit = crate::history::ReadLimit {
                options: controls,
                memory: Some(self.history_cache_capacity()),
                held: crate::history::READ_METADATA_BYTES,
            };
            let old = limit
                .lock(history)?
                .pins
                .get(name)
                .and_then(|pin| pin.text_index);
            let config = match old.and_then(|id| {
                read_pin(&root.join(ROOT).join(id.to_string()), resolved.commit.seq).ok()
            }) {
                Some(meta) => meta.config.with_dictionaries()?,
                None => self.active_text_pin_config()?,
            };
            let live = self.snapshot();
            let (snapshot, _, _) = self.materialize_snapshot(&at, limit, resolved, &live)?;
            let (id, bytes) = self.build_text_pin(&snapshot, config, controls)?;
            let mut history = history.lock();
            self.replace_text_pin(root, &mut history, name, Some(id), bytes)?;
            let generation = crate::commit::generation_number(&live.generation.name);
            Ok(self.named(&history, name, &history.pins[name], generation, w.head.seq))
        }
    }

    /// Remove a pin's index while retaining its RDF history. Returns whether the pin exists.
    pub fn drop_snapshot_text(&self, name: &str) -> Result<bool> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let (Some(root), Some(history)) = (&self.root, &self.history) else {
            return Ok(self.named_snapshot(name).is_some());
        };
        let _writer = self.guarded_writer();
        let mut history = history.lock();
        let Some(pin) = history.pins.get(name) else {
            return Ok(false);
        };
        if pin.text_index.is_some() {
            self.replace_text_pin(root, &mut history, name, None, 0)?;
        }
        Ok(true)
    }

    fn replace_text_pin(
        &self,
        root: &Path,
        history: &mut crate::history::HistoryState,
        name: &str,
        index: Option<uuid::Uuid>,
        bytes: u64,
    ) -> Result<()> {
        let pin = history.pins.get_mut(name).expect("pin exists under writer");
        let old = (pin.text_index, pin.text_bytes);
        (pin.text_index, pin.text_bytes) = (index, bytes);
        if let Err(error) = crate::history::write_file(
            root,
            self.dataset_id,
            &history.pins,
            history.retention,
            &history.schedules,
            history.catalog,
        ) {
            let pin = history.pins.get_mut(name).expect("pin exists under writer");
            (pin.text_index, pin.text_bytes) = old;
            // Startup reads the durable reference and removes the unreferenced directory.
            return Err(error);
        }
        self.remove_text_pin(old.0);
        Ok(())
    }

    #[cfg(feature = "text")]
    pub(super) fn active_text_pin_config(&self) -> Result<crate::text::TextConfig> {
        self.text
            .load()
            .as_ref()
            .map(|index| index.config().clone())
            .ok_or_else(|| {
                Error::invalid("enable full-text search before creating a snapshot text index")
            })
    }

    #[cfg(feature = "text")]
    pub(super) fn build_text_pin(
        &self,
        snapshot: &Snapshot,
        config: crate::text::TextConfig,
        options: &crate::history::HistoryOptions,
    ) -> Result<(uuid::Uuid, u64)> {
        use crate::history::ReadControl;
        let root = self.root.as_deref().expect("persistent snapshot");
        let parent = root.join(ROOT);
        std::fs::create_dir_all(&parent)?;
        let staged = tempfile::Builder::new()
            .prefix(".text-pin-")
            .tempdir_in(&parent)?;
        let stage = staged.path();
        let check = || {
            options.check_read()?;
            if let Some(reserve) = self.opts.min_free_disk_bytes {
                crate::disk::check_reserve(root, reserve, 0, false)?;
            }
            Ok(())
        };
        check()?;
        crate::text::TextIndex::build_pin(stage, &config, snapshot, &check)?;
        let metadata = TextPin {
            seq: snapshot.commit,
            config,
        };
        let metadata = crate::formats::settings_to_vec(META, &metadata)?;
        if metadata.len() as u64 > MAX_META_BYTES {
            return Err(Error::invalid("snapshot text configuration is too large"));
        }
        write_atomic(&stage.join(META), &metadata)?;
        sync_dir(stage)?;
        self.quota.check_rebuild(None, stage)?;
        check()?;
        let id = uuid::Uuid::new_v4();
        let path = parent.join(id.to_string());
        let bytes = dir_size(stage);
        std::fs::rename(stage, path)?;
        sync_dir(&parent)?;
        sync_dir(root)?;
        self.quota.invalidate();
        Ok((id, bytes))
    }

    #[cfg(feature = "text")]
    pub(super) fn attach_text_pin<C: HistoryRead>(
        &self,
        at: &crate::history::At,
        snapshot: &Arc<Snapshot>,
        limit: C,
    ) -> Result<Option<Arc<Snapshot>>> {
        let (Some(root), Some(history), crate::history::At::Snapshot(name)) =
            (&self.root, &self.history, at)
        else {
            return Ok(None);
        };
        let history = limit.read_limit().lock(history)?;
        let Some(id) = history
            .pins
            .get(name)
            .filter(|pin| pin.seq == snapshot.commit)
            .and_then(|pin| pin.text_index)
        else {
            return Ok(None);
        };
        let path = root.join(ROOT).join(id.to_string());
        // Check both the metadata scratch and the compressed Tantivy manifest before
        // admitting a reader. The search itself accounts its own working state.
        let estimate = [&path.join(META), &path.join("text/meta.json")]
            .into_iter()
            .map(|file| file.metadata().map_or(0, |m| m.len()))
            .try_fold(0u64, u64::checked_add);
        let opening = || {
            let Some(estimate) = estimate.filter(|size| *size <= MAX_META_BYTES) else {
                return Err(Error::Corrupt(
                    "snapshot text index metadata is too large".into(),
                ));
            };
            limit.check(estimate)?;
            let meta = read_pin(&path, snapshot.commit)?;
            crate::text::TextIndex::open_pin(&path, meta.config, meta.seq)
        };
        let mut pinned = (**snapshot).clone();
        pinned.text = match opening() {
            Ok(view) => {
                // Historical text uses the controlled collector even for a pin
                // that already has its own complete index.
                let mut base = (**snapshot).clone();
                base.text = Some(view.clone());
                Some(Arc::new(
                    view.with_historical_changes(snapshot.commit, Arc::new(base)),
                ))
            }
            Err(error @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(target: "sparkles::store", %error, %name, "snapshot text index unavailable");
                Some(crate::text::TextView::recovering(
                    snapshot.commit,
                    crate::text::next_owner(),
                    true,
                ))
            }
        };
        limit.check(0)?;
        Ok(Some(Arc::new(pinned)))
    }
}

#[cfg(feature = "text")]
fn read_pin(path: &Path, seq: u64) -> Result<TextPin> {
    let bytes = crate::formats::read_bounded(&path.join(META), MAX_META_BYTES)?
        .ok_or_else(|| Error::Corrupt("snapshot text configuration is too large".into()))?;
    let meta: TextPin = crate::formats::settings_from_slice(META, &bytes)?;
    if meta.seq != seq {
        return Err(Error::Corrupt(
            "snapshot text index names a different commit".into(),
        ));
    }
    Ok(meta)
}

/// Inspect pin index references and staging without opening a writer or collecting files.
pub(crate) fn probe(root: &Path, dataset_id: uuid::Uuid, full: bool) -> Vec<(String, String)> {
    #[cfg(not(feature = "text"))]
    let _ = full;
    let Ok(history) = crate::history::read_file(root, dataset_id) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let mut expected = std::collections::BTreeSet::new();
    for (name, pin) in history.pins {
        let Some(id) = pin.text_index else { continue };
        expected.insert(id);
        let rel = format!("{ROOT}/{id}");
        let path = root.join(&rel);
        #[cfg(feature = "text")]
        let checked = read_pin(&path, pin.seq).and_then(|meta| {
            crate::text::TextIndex::probe_pin(&path, &meta.config, meta.seq, full)
        });
        #[cfg(not(feature = "text"))]
        let checked = std::fs::metadata(path.join("text/meta.json"))
            .map(|_| ())
            .map_err(Error::from);
        if let Err(error) = checked {
            warnings.push((
                rel,
                format!("snapshot {name:?} text index is unavailable: {error}"),
            ));
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join(ROOT)) {
        for entry in entries.flatten() {
            let file = entry.file_name();
            let Some(name) = file.to_str() else { continue };
            let orphan = uuid::Uuid::parse_str(name).is_ok_and(|id| !expected.contains(&id));
            if orphan || name.starts_with(".text-pin-") {
                warnings.push((
                    format!("{ROOT}/{name}"),
                    "leftover snapshot text directory; removed on open".into(),
                ));
            }
        }
    }
    warnings
}

fn remove_dir(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
