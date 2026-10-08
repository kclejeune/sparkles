use super::{
    objects::EpochKeys,
    primitive,
    slots::{self, Descriptor, EncryptionOptions, MAX_SLOT_BYTES, MAX_SLOTS, Slot},
};
use crate::{
    BackupError, Code, OpenEnv, RepoConfig, Result,
    layout::{self, Marker},
    repo::{is_already_exists, is_not_found},
};
use futures::{StreamExt, TryStreamExt};
use object_store::{
    ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload, path::Path as Key,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
use uuid::Uuid;

pub(super) const INIT: &str = "sparkles-repo-init.json";
const MAX_INIT_BYTES: u64 = 1 << 20;

fn parse_marker(bytes: &[u8]) -> Result<Marker> {
    Marker::parse(bytes).map_err(|_| slots::incompatible())
}

pub(crate) struct Snapshot {
    pub descriptor: Descriptor,
    pub keys: BTreeMap<u32, Arc<EpochKeys>>,
}
impl Snapshot {
    pub fn active(&self) -> &EpochKeys {
        self.keys[&self.descriptor.active()].as_ref()
    }
    pub fn key(&self, epoch: u32) -> Result<&EpochKeys> {
        self.keys
            .get(&epoch)
            .map(Arc::as_ref)
            .ok_or_else(slots::required)
    }
    pub fn open_blob(&self, stored: &[u8], id: &str, size: u64) -> Result<Vec<u8>> {
        if stored.len() < 32 {
            return Err(super::objects::invalid("truncated encrypted blob header"));
        }
        let epoch = u32::from_le_bytes(stored[8..12].try_into().expect("header checked"));
        self.key(epoch)?.open_blob(stored, id, size)
    }
}

pub(crate) async fn read_bounded(
    store: &Arc<dyn ObjectStore>,
    key: &Key,
    limit: u64,
) -> Result<Vec<u8>> {
    read_bounded_optional(store, key, limit)
        .await?
        .ok_or_else(|| {
            BackupError::new(
                Code::RepositoryUnavailable,
                "required encrypted repository object is missing",
            )
        })
}

pub(super) async fn read_bounded_optional(
    store: &Arc<dyn ObjectStore>,
    key: &Key,
    limit: u64,
) -> Result<Option<Vec<u8>>> {
    let got = match store.get(key).await {
        Ok(got) => got,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if got.meta.size > limit {
        return Err(super::objects::invalid(
            "encrypted object exceeds stored-size limit",
        ));
    }
    let mut stream = got.into_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.try_next().await? {
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(super::objects::invalid(
                "encrypted object exceeds stored-size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

pub(crate) fn slot_key(id: Uuid) -> Key {
    Key::from(format!("keys/{id}.json"))
}
pub(crate) async fn read_slots(store: &Arc<dyn ObjectStore>) -> Result<Vec<Slot>> {
    let listed = store
        .list(Some(&Key::from("keys")))
        .take(MAX_SLOTS + 1)
        .try_collect::<Vec<_>>()
        .await?;
    if listed.len() > MAX_SLOTS {
        return Err(slots::config("too many key slots"));
    }
    let mut out = Vec::new();
    for meta in listed {
        let id = meta
            .location
            .as_ref()
            .strip_prefix("keys/")
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| slots::config("invalid key slot object name"))?;
        let Some(bytes) =
            read_bounded_optional(store, &meta.location, MAX_SLOT_BYTES as u64).await?
        else {
            continue;
        };
        let slot = Slot::parse(&bytes)?;
        if slot.id != id {
            return Err(slots::config(
                "key slot identity does not match its object name",
            ));
        }
        out.push(slot);
    }
    Ok(out)
}

pub(super) fn unlock(
    repository: Uuid,
    descriptor: Descriptor,
    slots: &[Slot],
    options: &EncryptionOptions,
    known: &BTreeMap<u32, Arc<EpochKeys>>,
) -> Result<Snapshot> {
    options.check(false)?;
    let epochs: HashSet<_> = descriptor.epochs.iter().map(|epoch| epoch.epoch).collect();
    // A passphrase label wraps at most one slot per epoch, which key addition
    // enforces. Refuse planted duplicates before any Argon2 work, so a writer to the
    // bucket cannot multiply the cost of an unlock.
    let mut phrases = HashSet::new();
    for slot in slots {
        if !epochs.contains(&slot.epoch) {
            return Err(slots::config("key slot refers to an unknown epoch"));
        }
        if slot.is_passphrase() && !phrases.insert((slot.epoch, slot.label.as_str())) {
            return Err(slots::config(
                "several passphrase slots share one label in an epoch",
            ));
        }
    }
    // Master keys already unlocked by this handle stay valid: an epoch's key never
    // changes, and every slot that wraps it is bound to the repository and epoch.
    let mut keys: BTreeMap<u32, Arc<EpochKeys>> = known
        .iter()
        .filter(|(epoch, _)| epochs.contains(epoch))
        .map(|(epoch, key)| (*epoch, key.clone()))
        .collect();
    for slot in slots {
        if slot.is_passphrase() && keys.contains_key(&slot.epoch) {
            continue;
        }
        if let Some(key) = slot.open(repository, options)? {
            if let Some(previous) = keys.get(&slot.epoch) {
                if previous.master.as_ref() != key.master.as_ref() {
                    return Err(slots::config("slots disagree on the repository master key"));
                }
            } else {
                keys.insert(slot.epoch, Arc::new(key));
            }
        }
    }
    let Some(active) = keys.get(&descriptor.active()) else {
        return Err(BackupError::new(
            Code::WrongRepositoryKey,
            "the supplied key opens no active repository slot",
        )
        .with(
            "expectedKekIds",
            slots
                .iter()
                .filter(|slot| slot.epoch == descriptor.active())
                .map(|slot| slot.kek_id.clone())
                .collect::<Vec<_>>(),
        ));
    };
    descriptor.verify(active)?;
    Ok(Snapshot { descriptor, keys })
}

pub(super) async fn unlock_async(
    repository: Uuid,
    descriptor: Descriptor,
    slots: Vec<Slot>,
    options: &EncryptionOptions,
    known: &BTreeMap<u32, Arc<EpochKeys>>,
) -> Result<Snapshot> {
    let options = options.clone();
    let known = known.clone();
    super::blocking(move || unlock(repository, descriptor, &slots, &options, &known)).await
}
pub(super) async fn wrap_slots(
    keys: Arc<EpochKeys>,
    options: &EncryptionOptions,
) -> Result<Vec<Slot>> {
    let options = options.clone();
    super::blocking(move || {
        let mut slots = Vec::new();
        for key in &options.keys {
            slots.push(Slot::local(&keys, key)?);
        }
        for key in &options.passphrases {
            slots.push(Slot::passphrase(&keys, key)?);
        }
        Ok(slots)
    })
    .await
}

/// Unlock the current marker's epochs. `dir` is the repository's local cache
/// directory, which holds the epoch floor. `known` holds master keys this handle
/// already unlocked, which are reused instead of opening their slots again.
pub(crate) async fn open_snapshot(
    store: &Arc<dyn ObjectStore>,
    id: Uuid,
    options: &EncryptionOptions,
    dir: Option<&std::path::Path>,
    known: &BTreeMap<u32, Arc<EpochKeys>>,
) -> Result<Snapshot> {
    for attempt in 0..8 {
        let marker =
            parse_marker(&read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?)
                .map_err(|_| slots::incompatible())?;
        if marker.id != id {
            return Err(slots::config("repository identity changed"));
        }
        let descriptor =
            Descriptor::parse(marker.encryption.as_ref().ok_or_else(slots::incompatible)?)?;
        super::floor::check(id, dir, &descriptor)?;
        let mut slots = read_slots(store).await?;
        if let Some(intent) = super::manage::read_rotation(store).await? {
            intent.check(id)?;
            if descriptor.active() == intent.previous_active() {
                // Only exactly registered immutable pending slots can be ignored while
                // the old epoch is still the publication visible to readers.
                for pending in &intent.slots {
                    if let Some(actual) = slots.iter().find(|slot| slot.id == pending.id)
                        && serde_json::to_vec(actual).ok() != serde_json::to_vec(pending).ok()
                    {
                        return Err(slots::config("pending rotation slot changed"));
                    }
                }
                slots.retain(|slot| !intent.slots.iter().any(|pending| pending.id == slot.id));
            }
        }
        if let Some(intent) = super::manage::read_retirement(store).await? {
            intent.check(id)?;
            if intent.published(&marker) {
                slots.retain(|slot| {
                    !(slot.epoch == intent.epoch && intent.slots.contains(&slot.id))
                });
            }
        }
        // Publication can complete between these reads for an unleased opener/list.
        // Re-read the marker and retry transient orphaned slot rows after intent cleanup.
        let latest = parse_marker(
            &read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?,
        )?;
        let listed = descriptor
            .epochs
            .iter()
            .map(|e| e.epoch)
            .collect::<HashSet<_>>();
        if latest != marker || slots.iter().any(|slot| !listed.contains(&slot.epoch)) {
            if attempt < 7 {
                tokio::task::yield_now().await;
                continue;
            }
            return Err(slots::config(
                "repository epoch metadata changed or contains an unknown slot",
            ));
        }
        let snapshot = unlock_async(id, descriptor, slots, options, known).await?;
        super::floor::record(id, dir, &snapshot.descriptor);
        return Ok(snapshot);
    }
    unreachable!("bounded loop returns")
}

/// The resolved key inputs of one repository handle and the master keys they
/// unlocked. Master keys are kept for the handle's lifetime and refreshed only when
/// the marker's descriptor changes, so passphrase derivation and protected-page
/// allocation happen once per epoch rather than once per operation.
pub(crate) struct KeyState {
    pub options: EncryptionOptions,
    cache: tokio::sync::Mutex<Option<Arc<Snapshot>>>,
}
impl std::ops::Deref for KeyState {
    type Target = EncryptionOptions;
    fn deref(&self) -> &EncryptionOptions {
        &self.options
    }
}
impl KeyState {
    pub fn new(options: EncryptionOptions) -> Self {
        Self {
            options,
            cache: tokio::sync::Mutex::new(None),
        }
    }
    /// The snapshot for the current marker. A cached snapshot is reused while the
    /// descriptor is unchanged. A changed descriptor is verified again in full.
    pub async fn snapshot(
        &self,
        store: &Arc<dyn ObjectStore>,
        id: Uuid,
        dir: Option<&std::path::Path>,
    ) -> Result<Arc<Snapshot>> {
        let marker = parse_marker(
            &read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?,
        )?;
        if marker.id != id {
            return Err(slots::config("repository identity changed"));
        }
        let descriptor =
            Descriptor::parse(marker.encryption.as_ref().ok_or_else(slots::incompatible)?)?;
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.descriptor == descriptor
        {
            return Ok(cached.clone());
        }
        let known = cache
            .as_ref()
            .map(|cached| cached.keys.clone())
            .unwrap_or_default();
        let fresh = Arc::new(open_snapshot(store, id, &self.options, dir, &known).await?);
        *cache = Some(fresh.clone());
        Ok(fresh)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    format: u32,
    kind: String,
    marker: Marker,
    slots: Vec<Slot>,
}

pub(crate) async fn attach_or_init(
    store: &Arc<dyn ObjectStore>,
    cfg: &RepoConfig,
    env: &OpenEnv,
    keys: &KeyState,
    native_root: Option<&(std::path::PathBuf, Arc<std::fs::File>)>,
) -> Result<Marker> {
    let options = &keys.options;
    options.check(false)?;
    let dir = |id: Uuid| env.cache_dir.as_ref().map(|d| d.join(id.to_string()));
    match store.get(&layout::marker_key()).await {
        Ok(got) => {
            if got.meta.size > MAX_SLOT_BYTES as u64 {
                return Err(slots::incompatible());
            }
            drop(got);
            let marker = parse_marker(
                &read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?,
            )?;
            if marker.encryption.as_ref().is_none_or(|v| v.is_null()) {
                return Err(slots::config(
                    "encryption cannot be enabled on an existing plaintext repository",
                ));
            }
            keys.snapshot(store, marker.id, dir(marker.id).as_deref())
                .await?;
            return Ok(marker);
        }
        Err(error) if is_not_found(&error) => {}
        Err(error) => return Err(error.into()),
    }
    if cfg.readonly || !env.init {
        return Err(BackupError::new(
            Code::NotARepository,
            "no encrypted repository marker",
        ));
    }
    if !cfg.conditional_writes {
        return Err(slots::config(
            "encrypted initialization requires conditional writes",
        ));
    }
    let intent_key = Key::from(INIT);
    let mut intent = read_bounded_optional(store, &intent_key, MAX_INIT_BYTES)
        .await?
        .map(|bytes| {
            serde_json::from_slice::<Intent>(&bytes)
                .map_err(|_| slots::config("invalid encrypted initialization intent"))
        })
        .transpose()?;
    if intent.is_none() && store.list(None).try_next().await?.is_some() {
        // Another initializer may publish between our first GET and LIST.
        if store.head(&layout::marker_key()).await.is_ok() {
            let marker = parse_marker(
                &read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?,
            )?;
            keys.snapshot(store, marker.id, dir(marker.id).as_deref())
                .await?;
            return Ok(marker);
        }
        match read_bounded_optional(store, &intent_key, MAX_INIT_BYTES).await? {
            Some(bytes) => {
                intent = Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|_| slots::config("invalid concurrent initialization intent"))?,
                )
            }
            None => {
                // Publication may finish and remove its intent after the marker probe.
                if let Some(bytes) =
                    read_bounded_optional(store, &layout::marker_key(), MAX_SLOT_BYTES as u64)
                        .await?
                {
                    let marker = parse_marker(&bytes)?;
                    keys.snapshot(store, marker.id, dir(marker.id).as_deref())
                        .await?;
                    return Ok(marker);
                }
                return Err(BackupError::new(
                    Code::NotARepository,
                    "repository prefix contains unrelated objects",
                ));
            }
        }
    }
    if intent.is_none() {
        options.check(true)?;
        let repository = Uuid::new_v4();
        let master = EpochKeys::new(repository, 1, primitive::random_key()?)?;
        let mut descriptor = Descriptor::new();
        descriptor.sign(&master)?;
        let slots = wrap_slots(Arc::new(master), options).await?;
        let mut marker = Marker::new(repository, crate::now_rfc3339());
        marker.hash = "hmac-sha256".into();
        marker.encryption = Some(serde_json::to_value(descriptor).expect("serializes"));
        let proposed = Intent {
            format: 1,
            kind: "sparkles-repo-initialization".into(),
            marker,
            slots,
        };
        let body = serde_json::to_vec(&proposed)
            .map_err(|_| slots::config("cannot serialize initialization intent"))?;
        match store
            .put_opts(
                &intent_key,
                PutPayload::from(body),
                PutOptions::from(PutMode::Create),
            )
            .await
        {
            Ok(_) => intent = Some(proposed),
            Err(error) if is_already_exists(&error) => {
                if let Some(bytes) =
                    read_bounded_optional(store, &intent_key, MAX_INIT_BYTES).await?
                {
                    intent =
                        Some(serde_json::from_slice(&bytes).map_err(|_| {
                            slots::config("invalid concurrent initialization intent")
                        })?);
                } else {
                    let marker = parse_marker(
                        &read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?,
                    )?;
                    keys.snapshot(store, marker.id, dir(marker.id).as_deref())
                        .await?;
                    return Ok(marker);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let intent = intent.expect("created or recovered");
    if intent.format != 1
        || intent.kind != "sparkles-repo-initialization"
        || intent.slots.is_empty()
        || intent.slots.len() > MAX_SLOTS
    {
        return Err(slots::config("invalid encrypted initialization intent"));
    }
    intent.marker.check().map_err(|_| slots::incompatible())?;
    let descriptor = Descriptor::parse(
        intent
            .marker
            .encryption
            .as_ref()
            .ok_or_else(slots::incompatible)?,
    )?;
    let mut checked = Vec::new();
    for slot in &intent.slots {
        checked.push(Slot::parse(
            &serde_json::to_vec(slot).map_err(|_| slots::config("invalid initialization slot"))?,
        )?);
    }
    unlock_async(
        intent.marker.id,
        descriptor,
        checked.clone(),
        options,
        &BTreeMap::new(),
    )
    .await?;
    let allowed: HashSet<_> = checked
        .iter()
        .map(|slot| slot_key(slot.id))
        .chain([intent_key.clone()])
        .collect();
    let mut listed = store.list(None);
    while let Some(meta) = listed.try_next().await? {
        if !allowed.contains(&meta.location) && meta.location != layout::marker_key() {
            return Err(BackupError::new(
                Code::NotARepository,
                "initialization prefix contains unrelated objects",
            ));
        }
    }
    drop(listed);
    for slot in checked {
        put_slot(store, &slot).await?;
    }
    match store
        .put_opts(
            &layout::marker_key(),
            PutPayload::from(intent.marker.to_bytes()),
            PutOptions::from(PutMode::Create),
        )
        .await
    {
        Ok(_) => {}
        Err(error) if is_already_exists(&error) => {}
        Err(error) => return Err(error.into()),
    }
    let winner =
        parse_marker(&read_bounded(store, &layout::marker_key(), MAX_SLOT_BYTES as u64).await?)
            .map_err(|_| slots::incompatible())?;
    keys.snapshot(store, winner.id, dir(winner.id).as_deref())
        .await?;
    if winner.id != intent.marker.id {
        return Err(slots::config("repository initialization identity changed"));
    }
    finalize_initialization(store, &intent_key, cfg, native_root).await?;
    Ok(winner)
}

async fn finalize_initialization(
    store: &Arc<dyn ObjectStore>,
    intent_key: &Key,
    cfg: &RepoConfig,
    native_root: Option<&(std::path::PathBuf, Arc<std::fs::File>)>,
) -> Result<()> {
    if let Some((root, directory)) = native_root {
        let (root, directory) = (root.clone(), directory.clone());
        let configured = cfg
            .path
            .clone()
            .ok_or_else(|| slots::config("native filesystem path missing"))?;
        tokio::task::spawn_blocking(move || {
            if std::fs::canonicalize(configured)? != root {
                return Err(slots::config(
                    "configured filesystem repository root changed",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let pinned = directory.metadata()?;
                let actual = std::fs::symlink_metadata(&root)?;
                if pinned.dev() != actual.dev() || pinned.ino() != actual.ino() {
                    return Err(slots::config(
                        "native filesystem repository root identity changed",
                    ));
                }
            }
            // Fence the current winner without rewriting a stale initial marker
            // over a concurrently rotated repository.
            super::manage::local::sync_directory(&root, &directory)
        })
        .await
        .map_err(|_| {
            BackupError::new(
                Code::Internal,
                "initialization directory sync worker failed",
            )
        })??;
    }
    let _ = store.delete(intent_key).await;
    Ok(())
}

pub(crate) async fn put_slot(store: &Arc<dyn ObjectStore>, slot: &Slot) -> Result<()> {
    let bytes = serde_json::to_vec(slot).map_err(|_| slots::config("cannot serialize key slot"))?;
    match store
        .put_opts(
            &slot_key(slot.id),
            PutPayload::from(bytes.clone()),
            PutOptions::from(PutMode::Create),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_already_exists(&error) => {
            if read_bounded(store, &slot_key(slot.id), MAX_SLOT_BYTES as u64).await? == bytes {
                Ok(())
            } else {
                Err(slots::config(
                    "immutable key slot already exists with different contents",
                ))
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_initializer_cleanup_retries_directory_sync_without_rewriting_winner() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = RepoConfig {
            name: "native".into(),
            kind: crate::RepoType::Fs,
            path: Some(tmp.path().to_str().unwrap().into()),
            conditional_writes: true,
            ..Default::default()
        };
        let options = super::super::tests::options(85);
        let (entered, release) = super::super::manage::local::pause_sync_failure(tmp.path());
        let proposed_cfg = cfg.clone();
        let proposed_options = options.clone();
        let task = tokio::spawn(async move {
            crate::Repository::open_encrypted(&proposed_cfg, &OpenEnv::default(), &proposed_options)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        assert!(tmp.path().join(INIT).exists());
        assert!(tmp.path().join(layout::marker_key().as_ref()).exists());
        release.send(()).unwrap();
        assert!(
            task.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("directory sync test failure")
        );
        assert!(tmp.path().join(INIT).exists());
        let repo = Arc::new(
            crate::Repository::open_encrypted(&cfg, &OpenEnv::default(), &options)
                .await
                .unwrap(),
        );
        // Existing-marker open keeps INIT; a competing manager may rotate before
        // the bottom initializer retries its durable cleanup.
        assert!(tmp.path().join(INIT).exists());
        repo.key_rotate(&crate::Ctl::default()).await.unwrap();
        let winner = read_bounded(&repo.store, &layout::marker_key(), MAX_SLOT_BYTES as u64)
            .await
            .unwrap();
        let (entered, release) = super::super::manage::local::pause_sync(tmp.path());
        let task_repo = repo.clone();
        let cleanup = tokio::spawn(async move {
            finalize_initialization(
                &task_repo.store,
                &Key::from(INIT),
                &task_repo.config,
                task_repo.native_root.as_ref(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        assert!(tmp.path().join(INIT).exists());
        assert_eq!(
            read_bounded(&repo.store, &layout::marker_key(), MAX_SLOT_BYTES as u64)
                .await
                .unwrap(),
            winner
        );
        release.send(()).unwrap();
        cleanup.await.unwrap().unwrap();
        assert!(!tmp.path().join(INIT).exists());
        assert_eq!(
            read_bounded(&repo.store, &layout::marker_key(), MAX_SLOT_BYTES as u64)
                .await
                .unwrap(),
            winner
        );
        assert_eq!(repo.security().await.unwrap().epoch(), Some(2));
    }
    use object_store::memory::InMemory;
    #[tokio::test]
    async fn interrupted_initialization_recovers_every_slot_publication_boundary() {
        for published in 0..=2 {
            let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
            let mut options = super::super::tests::options(31);
            options.keys.push(
                slots::LocalKey::new("recovery", slots::LocalKeySource::File, [32; 32]).unwrap(),
            );
            let id = Uuid::new_v4();
            let keys = EpochKeys::new(id, 1, primitive::random_key().unwrap()).unwrap();
            let mut marker = Marker::new(id, crate::now_rfc3339());
            marker.hash = "hmac-sha256".into();
            marker.encryption = Some(serde_json::to_value(Descriptor::new()).unwrap());
            let slots = options
                .keys
                .iter()
                .map(|key| Slot::local(&keys, key).unwrap())
                .collect::<Vec<_>>();
            let intent = Intent {
                format: 1,
                kind: "sparkles-repo-initialization".into(),
                marker: marker.clone(),
                slots: slots.clone(),
            };
            store
                .put(
                    &Key::from(INIT),
                    PutPayload::from(serde_json::to_vec(&intent).unwrap()),
                )
                .await
                .unwrap();
            for slot in slots.iter().take(published) {
                put_slot(&store, slot).await.unwrap();
            }
            let cfg = RepoConfig {
                name: "recover".into(),
                kind: crate::RepoType::Memory,
                conditional_writes: true,
                ..Default::default()
            };
            let env = OpenEnv {
                store: Some(store.clone()),
                ..Default::default()
            };
            let wrong = super::super::tests::options(33);
            assert!(
                attach_or_init(&store, &cfg, &env, &KeyState::new(wrong), None)
                    .await
                    .is_err()
            );
            assert!(store.head(&layout::marker_key()).await.is_err());
            let recovered =
                attach_or_init(&store, &cfg, &env, &KeyState::new(options.clone()), None)
                    .await
                    .unwrap();
            assert_eq!(recovered.id, id);
            assert_eq!(read_slots(&store).await.unwrap().len(), 2);
            assert!(store.head(&Key::from(INIT)).await.is_err());
        }
    }
}
