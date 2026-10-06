//! Exclusive key management. Rotation publishes immutable wrapped slots before
//! changing the marker; a durable intent makes every interruption recoverable.
use super::{
    objects::{EpochKeys, MAX_PIECE_BYTES, stored_blob_limit},
    primitive,
    repository::{self},
    slots::{
        self, Descriptor, EncryptionOptions, Epoch, KeySlotSummary, LocalKey, Passphrase, Slot,
    },
};
use crate::{
    BackupError, Code, Ctl, LockKind, LockOperation, Repository, Result,
    layout::{self, Marker},
    lock,
};
use futures::TryStreamExt;
use object_store::{
    ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload, UpdateVersion, path::Path as Key,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
use zeroize::Zeroizing;

#[path = "local.rs"]
pub(super) mod local;

struct KeyGuard {
    lease: Arc<lock::LockGuard>,
    local: Option<local::Guard>,
    owned: crate::LockObject,
}
impl KeyGuard {
    async fn release(self) -> Result<()> {
        match Arc::try_unwrap(self.lease) {
            Ok(lease) => lease.release().await,
            // A dropped publication can still own this lease while syncing.
            Err(lease) => {
                drop(lease);
                Ok(())
            }
        }
    }
    async fn finish<T>(self, result: Result<T>) -> Result<T> {
        // Explicit cleanup survives short-lived caller runtimes on error as well
        // as success. Preserve the operation error if release also fails.
        let released = self.release().await;
        result.and_then(|value| released.map(|()| value))
    }
}

const ROTATION: &str = "sparkles-repo-rotation.json";
const MAX_ROTATION_BYTES: u64 = 1 << 20;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rotation {
    format: u32,
    kind: String,
    previous: Marker,
    next: Marker,
    pub slots: Vec<Slot>,
}
impl Rotation {
    pub fn previous_active(&self) -> u32 {
        Descriptor::parse(self.previous.encryption.as_ref().expect("validated"))
            .expect("validated")
            .active()
    }
    pub fn check(&self, id: Uuid) -> Result<()> {
        if self.format != 1
            || self.kind != "sparkles-repo-rotation"
            || self.previous.id != id
            || self.next.id != id
            || self.slots.is_empty()
            || self.slots.len() > slots::MAX_SLOTS
        {
            return Err(slots::config("invalid rotation intent"));
        }
        self.previous.check().map_err(|_| slots::incompatible())?;
        self.next.check().map_err(|_| slots::incompatible())?;
        let mut unchanged = self.previous.clone();
        unchanged.encryption = self.next.encryption.clone();
        if unchanged != self.next {
            return Err(slots::config(
                "key intent changes immutable repository metadata",
            ));
        }
        let old = Descriptor::parse(
            self.previous
                .encryption
                .as_ref()
                .ok_or_else(slots::incompatible)?,
        )?;
        let new = Descriptor::parse(
            self.next
                .encryption
                .as_ref()
                .ok_or_else(slots::incompatible)?,
        )?;
        if new.active() <= old.epochs.iter().map(|e| e.epoch).max().unwrap_or(0)
            || new.epochs.len() != old.epochs.len() + 1
            || old.epochs.iter().any(|e| {
                !new.epochs
                    .iter()
                    .any(|n| n.epoch == e.epoch && n.state == "retired" && n.created == e.created)
            })
        {
            return Err(slots::config(
                "rotation epochs do not extend the previous marker",
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for slot in &self.slots {
            Slot::parse(
                &serde_json::to_vec(slot).map_err(|_| slots::config("invalid rotation slot"))?,
            )?;
            if slot.epoch != new.active() || !ids.insert(slot.id) {
                return Err(slots::config("invalid rotation slot epoch or identity"));
            }
        }
        Ok(())
    }
}
pub(super) async fn read_rotation(store: &Arc<dyn ObjectStore>) -> Result<Option<Rotation>> {
    repository::read_bounded_optional(store, &Key::from(ROTATION), MAX_ROTATION_BYTES)
        .await?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| slots::config("invalid rotation intent"))
        })
        .transpose()
}

const RETIREMENT: &str = "sparkles-repo-retirement.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retirement {
    previous: Marker,
    next: Marker,
    pub epoch: u32,
    pub slots: Vec<Uuid>,
}
impl Retirement {
    pub fn check(&self, id: Uuid) -> Result<()> {
        self.previous.check().map_err(|_| slots::incompatible())?;
        self.next.check().map_err(|_| slots::incompatible())?;
        let mut unchanged = self.previous.clone();
        unchanged.encryption = self.next.encryption.clone();
        if unchanged != self.next {
            return Err(slots::config(
                "key intent changes immutable repository metadata",
            ));
        }
        let old = Descriptor::parse(
            self.previous
                .encryption
                .as_ref()
                .ok_or_else(slots::incompatible)?,
        )?;
        let new = Descriptor::parse(
            self.next
                .encryption
                .as_ref()
                .ok_or_else(slots::incompatible)?,
        )?;
        let mut expected = old.clone();
        expected.epochs.retain(|e| e.epoch != self.epoch);
        if self.previous.id != id
            || self.next.id != id
            || self.epoch == old.active()
            || old.epochs.len() != new.epochs.len() + 1
            || self.slots.is_empty()
            || self.slots.len() > slots::MAX_SLOTS
            || self
                .slots
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.slots.len()
            || serde_json::to_value(expected).ok() != serde_json::to_value(new).ok()
        {
            return Err(slots::config("invalid epoch retirement intent"));
        }
        Ok(())
    }
    pub fn published(&self, current: &Marker) -> bool {
        current.to_bytes() == self.next.to_bytes()
    }
}
pub(super) async fn read_retirement(store: &Arc<dyn ObjectStore>) -> Result<Option<Retirement>> {
    repository::read_bounded_optional(store, &Key::from(RETIREMENT), MAX_ROTATION_BYTES)
        .await?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| slots::config("invalid retirement intent"))
        })
        .transpose()
}

async fn retirement_targets(repo: &Repository, intent: &Retirement) -> Result<Vec<Slot>> {
    let mut targets = Vec::new();
    for id in &intent.slots {
        if let Some(bytes) = repository::read_bounded_optional(
            &repo.store,
            &repository::slot_key(*id),
            slots::MAX_SLOT_BYTES as u64,
        )
        .await?
        {
            let slot = Slot::parse(&bytes)?;
            if slot.id != *id || slot.epoch != intent.epoch {
                return Err(slots::config(
                    "retirement target does not belong to the retired epoch",
                ));
            }
            targets.push(slot);
        }
    }
    let options = repo.encryption_options()?.clone();
    let repository = repo.marker.id;
    super::blocking(move || {
        for slot in &targets {
            // Offline recovery credentials need not be present to retire an epoch.
            // Authenticate available wraps; validate all target identities before deletion.
            let _ = slot.open(repository, &options)?;
        }
        Ok(targets)
    })
    .await
}

async fn recover_retirement(repo: &Repository, guard: &KeyGuard, ctl: &Ctl) -> Result<()> {
    let Some(intent) = read_retirement(&repo.store).await? else {
        return Ok(());
    };
    intent.check(repo.marker.id)?;
    let (current, _) = marker(&repo.store).await?;
    if !intent.published(&current) {
        if current.to_bytes() != intent.previous.to_bytes() {
            return Err(slots::config("retirement intent disagrees with marker"));
        }
        // The authenticated no-reference proof was made while this exclusive lease
        // was held. On a pre-publication crash abandon it and perform a fresh proof.
        repo.store.delete(&Key::from(RETIREMENT)).await?;
        return Ok(());
    }
    if guard.local.is_some() {
        publish(repo, guard, &intent.previous, &intent.next, ctl).await?;
    }
    let targets = retirement_targets(repo, &intent).await?;
    for slot in targets {
        ctl.check()?;
        repo.store.delete(&repository::slot_key(slot.id)).await?;
    }
    repo.store.delete(&Key::from(RETIREMENT)).await?;
    Ok(())
}

async fn marker(store: &Arc<dyn ObjectStore>) -> Result<(Marker, UpdateVersion)> {
    let got = store.get(&layout::marker_key()).await?;
    if got.meta.size > slots::MAX_SLOT_BYTES as u64 {
        return Err(slots::incompatible());
    }
    let version = UpdateVersion {
        e_tag: got.meta.e_tag.clone(),
        version: got.meta.version.clone(),
    };
    let mut bytes = Vec::new();
    let mut stream = got.into_stream();
    while let Some(chunk) = stream.try_next().await? {
        if bytes.len().saturating_add(chunk.len()) > slots::MAX_SLOT_BYTES {
            return Err(slots::incompatible());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((
        Marker::parse(&bytes).map_err(|_| slots::incompatible())?,
        version,
    ))
}
async fn publish(
    repo: &Repository,
    guard: &KeyGuard,
    previous: &Marker,
    next: &Marker,
    ctl: &Ctl,
) -> Result<()> {
    let prepared = if let Some(local) = &guard.local {
        Some(local.prepare(next, ctl).await?)
    } else {
        None
    };
    let (current, version) = marker(&repo.store).await?;
    if let Some(local) = &guard.local {
        local.verify_lease(repo, &guard.lease, &guard.owned).await?;
    }
    ctl.check()?;
    if current.to_bytes() == next.to_bytes() {
        // An earlier rename may have succeeded while directory sync failed. Native
        // recovery must establish durability before it can remove the intent.
        if let Some(prepared) = prepared {
            return prepared.publish(ctl, guard.lease.clone()).await;
        }
        return Ok(());
    }
    if current.to_bytes() != previous.to_bytes() {
        return Err(slots::config(
            "repository marker changed during key management",
        ));
    }
    if let Some(prepared) = prepared {
        return prepared.publish(ctl, guard.lease.clone()).await;
    }
    repo.store
        .put_opts(
            &layout::marker_key(),
            PutPayload::from(next.to_bytes()),
            PutOptions::from(PutMode::Update(version)),
        )
        .await?;
    Ok(())
}

async fn recover(
    repo: &Repository,
    guard: &KeyGuard,
    options: &EncryptionOptions,
    ctl: &Ctl,
) -> Result<bool> {
    let Some(intent) = read_rotation(&repo.store).await? else {
        return Ok(false);
    };
    intent.check(repo.marker.id)?;
    let (current, _) = marker(&repo.store).await?;
    if current.to_bytes() != intent.previous.to_bytes()
        && current.to_bytes() != intent.next.to_bytes()
    {
        return Err(slots::config(
            "rotation intent disagrees with repository marker",
        ));
    }
    let mut slots = repository::read_slots(&repo.store).await?;
    for proposed in &intent.slots {
        if let Some(actual) = slots.iter().find(|slot| slot.id == proposed.id) {
            if serde_json::to_vec(actual).ok() != serde_json::to_vec(proposed).ok() {
                return Err(slots::config("pending rotation slot changed"));
            }
        } else {
            slots.push(proposed.clone());
        }
    }
    repository::unlock_async(
        repo.marker.id,
        Descriptor::parse(
            intent
                .next
                .encryption
                .as_ref()
                .ok_or_else(slots::incompatible)?,
        )?,
        slots,
        options,
    )
    .await?;
    for slot in &intent.slots {
        ctl.check()?;
        repository::put_slot(&repo.store, slot).await?;
    }
    ctl.check()?;
    publish(repo, guard, &intent.previous, &intent.next, ctl).await?;
    repo.store.delete(&Key::from(ROTATION)).await?;
    Ok(true)
}

impl Repository {
    fn encryption_options(&self) -> Result<&EncryptionOptions> {
        match &self.key_options {
            crate::security::KeyOptions::Local(options) => Ok(options),
            _ => Err(slots::config("repository is not encrypted")),
        }
    }
    async fn key_guard(&self, operation: LockOperation, ctl: &Ctl) -> Result<KeyGuard> {
        if self.readonly() {
            return Err(crate::repo::read_only(&self.config.name));
        }
        if !self.config.conditional_writes {
            return Err(slots::config("key management requires conditional writes"));
        }
        let options = self.encryption_options()?;
        let local = local::Guard::acquire(self, ctl).await?;
        let lease = lock::acquire(self, LockKind::Exclusive, operation, ctl).await?;
        let checked = async {
            let key = layout::lock_key(
                lease
                    .id()
                    .ok_or_else(|| slots::config("exclusive key lease is missing"))?,
            );
            let bytes =
                repository::read_bounded(&self.store, &key, slots::MAX_SLOT_BYTES as u64).await?;
            let owned: crate::LockObject = serde_json::from_slice(&bytes)
                .map_err(|_| slots::config("invalid exclusive key lease"))?;
            if owned.kind != LockKind::Exclusive
                || owned.holder.pid != std::process::id()
                || owned.holder.server != self.env.server_id
                || owned.operation != operation
            {
                return Err(slots::config("exclusive key lease ownership changed"));
            }
            Ok(owned)
        }
        .await;
        let owned = match checked {
            Ok(owned) => owned,
            Err(error) => {
                let _ = lease.release().await;
                return Err(error);
            }
        };
        let guard = KeyGuard {
            lease: Arc::new(lease),
            local,
            owned,
        };
        let recovered = async {
            recover_retirement(self, &guard, ctl).await?;
            if !matches!(operation, LockOperation::KeyRotate) {
                recover(self, &guard, options, ctl).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = recovered {
            return guard.finish(Err(error)).await;
        }
        Ok(guard)
    }
    /// Public metadata only; wrapped keys, nonces and KDF secrets are excluded.
    pub async fn key_list(&self) -> Result<Vec<KeySlotSummary>> {
        self.encryption_options()?;
        self.security().await?;
        let mut slots: Vec<_> = repository::read_slots(&self.store)
            .await?
            .iter()
            .map(Slot::summary)
            .collect();
        slots.sort_by_key(|s| (s.epoch, s.id));
        Ok(slots)
    }
    /// Add an independent recovery key to every readable epoch, without changing IDs.
    pub async fn key_add(&self, key: &LocalKey, ctl: &Ctl) -> Result<Vec<KeySlotSummary>> {
        self.key_add_input(
            key.label.clone(),
            EncryptionOptions {
                keys: vec![key.clone()],
                single_key_ok: true,
                ..Default::default()
            },
            ctl,
        )
        .await
    }
    /// Add a passphrase recovery slot to every readable epoch. Retries authenticate
    /// existing labels using their stored salt instead of creating duplicate slots.
    pub async fn key_add_passphrase(
        &self,
        key: &Passphrase,
        ctl: &Ctl,
    ) -> Result<Vec<KeySlotSummary>> {
        self.key_add_input(
            key.label.clone(),
            EncryptionOptions {
                passphrases: vec![key.clone()],
                single_key_ok: true,
                ..Default::default()
            },
            ctl,
        )
        .await
    }
    async fn key_add_input(
        &self,
        label: String,
        supplied: EncryptionOptions,
        ctl: &Ctl,
    ) -> Result<Vec<KeySlotSummary>> {
        supplied.check(false)?;
        let guard = self.key_guard(LockOperation::KeyAdd, ctl).await?;
        let result = async {
            let options = self.encryption_options()?;
            let snapshot = repository::open_snapshot(&self.store, self.marker.id, options).await?;
            if snapshot.keys.len() != snapshot.descriptor.epochs.len() {
                return Err(slots::required());
            }
            let existing = repository::read_slots(&self.store).await?;
            let repository = self.marker.id;
            let count = existing.len();
            let planned = super::blocking(move || {
                let mut planned = Vec::new();
                for keys in snapshot.keys.values() {
                    let prior = existing
                        .iter()
                        .filter(|slot| slot.label == label && slot.epoch == keys.epoch)
                        .collect::<Vec<_>>();
                    if prior.len() > 1 {
                        return Err(slots::config("duplicate key labels in an epoch"));
                    }
                    if let Some(prior) = prior.first() {
                        let recovered = prior.open(repository, &supplied)?.ok_or_else(|| {
                            slots::config("key label already exists for a different key")
                        })?;
                        if recovered.master.as_ref() != keys.master.as_ref() {
                            return Err(slots::config("key label wraps a different master key"));
                        }
                        planned.push(((*prior).clone(), false));
                    } else {
                        let slot = if let Some(key) = supplied.keys.first() {
                            Slot::local(keys, key)?
                        } else {
                            Slot::passphrase(keys, &supplied.passphrases[0])?
                        };
                        planned.push((slot, true));
                    }
                }
                Ok(planned)
            })
            .await?;
            if count + planned.iter().filter(|(_, new)| *new).count() > slots::MAX_SLOTS {
                return Err(slots::config("too many key slots"));
            }
            let mut out = Vec::new();
            for (slot, new) in planned {
                ctl.check()?;
                if new {
                    repository::put_slot(&self.store, &slot).await?;
                }
                out.push(slot.summary());
            }
            Ok(out)
        }
        .await;
        guard.finish(result).await
    }
    /// Refuse removing an epoch's last slot, including historical readable epochs.
    pub async fn key_remove(&self, id: Uuid, ctl: &Ctl) -> Result<bool> {
        let guard = self.key_guard(LockOperation::KeyRemove, ctl).await?;
        let result = async {
            let snapshot = self.security().await?.sealed.expect("encrypted");
            let slots = repository::read_slots(&self.store).await?;
            let Some(remove) = slots.iter().find(|s| s.id == id) else {
                return Ok(false);
            };
            if slots.iter().filter(|s| s.epoch == remove.epoch).count() <= 1 {
                return Err(BackupError::new(
                    Code::LastKeySlot,
                    "cannot remove an epoch's last recovery slot",
                ));
            }
            // Require a remaining slot usable by this caller, keeping this handle useful.
            let options = self.encryption_options()?.clone();
            let repository = self.marker.id;
            let remaining = slots
                .iter()
                .filter(|s| s.epoch == remove.epoch && s.id != id)
                .cloned()
                .collect::<Vec<_>>();
            let usable = super::blocking(move || {
                Ok(remaining
                    .iter()
                    .map(|slot| slot.open(repository, &options))
                    .collect::<Result<Vec<_>>>()?
                    .iter()
                    .any(Option::is_some))
            })
            .await?;
            if !usable {
                return Err(BackupError::new(
                    Code::LastKeySlot,
                    "no remaining usable recovery slot for this epoch",
                ));
            }
            snapshot.key(remove.epoch)?;
            ctl.check()?;
            self.store.delete(&repository::slot_key(id)).await?;
            Ok(true)
        }
        .await;
        guard.finish(result).await
    }
    /// Publish a new master-key epoch. A repeated call resumes an interrupted rotation.
    pub async fn key_rotate(&self, ctl: &Ctl) -> Result<u32> {
        let guard = self.key_guard(LockOperation::KeyRotate, ctl).await?;
        let result = async {
            let options = self.encryption_options()?;
            if recover(self, &guard, options, ctl).await? {
                let epoch = self.security().await?.epoch().expect("encrypted");
                return Ok(epoch);
            }
            let snapshot = repository::open_snapshot(&self.store, self.marker.id, options).await?;
            let existing = repository::read_slots(&self.store).await?;
            let count = options.keys.len() + options.passphrases.len();
            if existing.len() + count > slots::MAX_SLOTS
                || snapshot.descriptor.epochs.len() >= slots::MAX_SLOTS
            {
                return Err(slots::config("too many key slots or epochs"));
            }
            let epoch = snapshot
                .descriptor
                .epochs
                .iter()
                .map(|e| e.epoch)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| slots::config("epoch number exhausted"))?;
            let keys = EpochKeys::new(
                self.marker.id,
                epoch,
                Zeroizing::new(primitive::random::<32>()?),
            )?;
            let slots = repository::wrap_slots(Arc::new(keys), options).await?;
            let (previous, _) = marker(&self.store).await?;
            let mut next = previous.clone();
            let mut descriptor = snapshot.descriptor;
            for old in &mut descriptor.epochs {
                old.state = "retired".into();
            }
            descriptor.epochs.push(Epoch {
                epoch,
                state: "active".into(),
                created: crate::now_rfc3339(),
            });
            next.encryption = Some(serde_json::to_value(descriptor).expect("serializes"));
            let intent = Rotation {
                format: 1,
                kind: "sparkles-repo-rotation".into(),
                previous,
                next,
                slots,
            };
            intent.check(self.marker.id)?;
            ctl.check()?;
            self.store
                .put_opts(
                    &Key::from(ROTATION),
                    PutPayload::from(
                        serde_json::to_vec(&intent)
                            .map_err(|_| slots::config("cannot serialize rotation intent"))?,
                    ),
                    PutOptions::from(PutMode::Create),
                )
                .await?;
            recover(self, &guard, options, ctl).await?;
            Ok(epoch)
        }
        .await;
        guard.finish(result).await
    }
    /// Remove a retired epoch only after every manifest and orphan blob authenticates
    /// and none needs it. Corruption or an unavailable historical key stops retirement.
    pub async fn key_retire(&self, epoch: u32, ctl: &Ctl) -> Result<()> {
        let guard = self.key_guard(LockOperation::KeyRetire, ctl).await?;
        let result = async {
            let snapshot = self.security().await?.sealed.expect("encrypted");
            if epoch == snapshot.descriptor.active()
                || !snapshot.descriptor.epochs.iter().any(|e| e.epoch == epoch)
            {
                return Err(slots::config(
                    "only a registered retired epoch can be removed",
                ));
            }
            let manifests = self
                .scan_manifests()
                .await?
                .0
                .into_iter()
                .map(|entry| entry.manifest)
                .collect::<Result<Vec<_>>>()?;
            for manifest in &manifests {
                crate::manifest::validate(
                    manifest,
                    self.marker.piece_bytes,
                    sparkles_core::builder::FORMAT_VERSION,
                )?;
            }
            if manifests.iter().any(|m| {
                m.encryption
                    .as_ref()
                    .and_then(|e| e.get("epoch"))
                    .and_then(|e| e.as_u64())
                    == Some(epoch as u64)
            }) {
                return Err(slots::config("epoch is still required by a backup"));
            }
            let listed = self
                .store
                .list(Some(&Key::from("blobs")))
                .try_collect::<Vec<_>>()
                .await?;
            for meta in listed {
                ctl.check()?;
                let id = layout::blob_id_of(&meta.location).ok_or_else(|| {
                    slots::config("invalid blob object name during epoch retirement")
                })?;
                let bytes = repository::read_bounded(
                    &self.store,
                    &meta.location,
                    stored_blob_limit(MAX_PIECE_BYTES)?,
                )
                .await?;
                if bytes.len() < 32 {
                    return Err(super::objects::invalid("truncated encrypted orphan"));
                }
                let blob_epoch = u32::from_le_bytes(bytes[8..12].try_into().expect("checked"));
                snapshot.key(blob_epoch)?.check_orphan(&bytes, id)?;
                if blob_epoch == epoch {
                    return Err(slots::config(
                        "epoch is still required by an orphan blob; run GC first",
                    ));
                }
            }
            let (previous, _) = marker(&self.store).await?;
            let mut next = previous.clone();
            let mut descriptor = snapshot.descriptor.clone();
            descriptor.epochs.retain(|e| e.epoch != epoch);
            next.encryption = Some(serde_json::to_value(descriptor).expect("serializes"));
            let retired = repository::read_slots(&self.store)
                .await?
                .into_iter()
                .filter(|slot| slot.epoch == epoch)
                .map(|slot| slot.id)
                .collect();
            let intent = Retirement {
                previous,
                next,
                epoch,
                slots: retired,
            };
            intent.check(self.marker.id)?;
            retirement_targets(self, &intent).await?;
            ctl.check()?;
            self.store
                .put_opts(
                    &Key::from(RETIREMENT),
                    PutPayload::from(
                        serde_json::to_vec(&intent)
                            .map_err(|_| slots::config("cannot serialize retirement intent"))?,
                    ),
                    PutOptions::from(PutMode::Create),
                )
                .await?;
            ctl.check()?;
            publish(self, &guard, &intent.previous, &intent.next, ctl).await?;
            recover_retirement(self, &guard, ctl).await?;
            Ok(())
        }
        .await;
        guard.finish(result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    async fn filesystem(path: &std::path::Path, options: &EncryptionOptions) -> Repository {
        Repository::open_encrypted(
            &crate::RepoConfig {
                name: "encrypted".into(),
                kind: crate::RepoType::Fs,
                path: Some(path.to_str().unwrap().into()),
                conditional_writes: true,
                ..Default::default()
            },
            &crate::OpenEnv {
                lock_wait: std::time::Duration::from_secs(3),
                ..Default::default()
            },
            options,
        )
        .await
        .unwrap()
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_rotation_retirement_recovery_and_reopen() {
        for published in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let opts = super::super::tests::options(71);
            let repo = filesystem(tmp.path(), &opts).await;
            let guard = repo
                .key_guard(LockOperation::KeyRotate, &Ctl::default())
                .await
                .unwrap();
            let (previous, _) = marker(&repo.store).await.unwrap();
            let mut next = previous.clone();
            let mut descriptor = Descriptor::parse(previous.encryption.as_ref().unwrap()).unwrap();
            descriptor.epochs[0].state = "retired".into();
            descriptor.epochs.push(Epoch {
                epoch: 2,
                state: "active".into(),
                created: crate::now_rfc3339(),
            });
            next.encryption = Some(serde_json::to_value(descriptor).unwrap());
            let keys = EpochKeys::new(
                repo.id(),
                2,
                Zeroizing::new(primitive::random::<32>().unwrap()),
            )
            .unwrap();
            let intent = Rotation {
                format: 1,
                kind: "sparkles-repo-rotation".into(),
                previous,
                next,
                slots: vec![Slot::local(&keys, &opts.keys[0]).unwrap()],
            };
            repo.store
                .put(
                    &Key::from(ROTATION),
                    PutPayload::from(serde_json::to_vec(&intent).unwrap()),
                )
                .await
                .unwrap();
            repository::put_slot(&repo.store, &intent.slots[0])
                .await
                .unwrap();
            if published {
                publish(
                    &repo,
                    &guard,
                    &intent.previous,
                    &intent.next,
                    &Ctl::default(),
                )
                .await
                .unwrap();
            }
            guard.release().await.unwrap();
            drop(repo);
            let repo = filesystem(tmp.path(), &opts).await;
            assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
            assert!(repo.store.head(&Key::from(ROTATION)).await.is_err());
            let guard = repo
                .key_guard(LockOperation::KeyRotate, &Ctl::default())
                .await
                .unwrap();
            let (previous, _) = marker(&repo.store).await.unwrap();
            let mut next = previous.clone();
            let mut descriptor = Descriptor::parse(previous.encryption.as_ref().unwrap()).unwrap();
            descriptor.epochs.retain(|e| e.epoch != 1);
            next.encryption = Some(serde_json::to_value(descriptor).unwrap());
            let intent = Retirement {
                previous,
                next,
                epoch: 1,
                slots: repository::read_slots(&repo.store)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|s| s.epoch == 1)
                    .map(|s| s.id)
                    .collect(),
            };
            repo.store
                .put(
                    &Key::from(RETIREMENT),
                    PutPayload::from(serde_json::to_vec(&intent).unwrap()),
                )
                .await
                .unwrap();
            if published {
                publish(
                    &repo,
                    &guard,
                    &intent.previous,
                    &intent.next,
                    &Ctl::default(),
                )
                .await
                .unwrap();
            }
            guard.release().await.unwrap();
            drop(repo);
            let repo = filesystem(tmp.path(), &opts).await;
            if published {
                repo.key_add(
                    &slots::LocalKey::new("recovery", slots::LocalKeySource::File, [72; 32])
                        .unwrap(),
                    &Ctl::default(),
                )
                .await
                .unwrap();
            } else {
                repo.key_retire(1, &Ctl::default()).await.unwrap();
            }
            assert!(repo.store.head(&Key::from(RETIREMENT)).await.is_err());
            drop(repo);
            let reopened = filesystem(tmp.path(), &opts).await;
            assert_eq!(reopened.security().await.unwrap().epoch(), Some(2));
            assert!(
                reopened
                    .key_list()
                    .await
                    .unwrap()
                    .iter()
                    .all(|s| s.epoch == 2)
            );
            assert!(!std::fs::read_dir(tmp.path()).unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sparkles-marker-")
            }));
        }
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_serializes_rotations_and_refuses_lost_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = super::super::tests::options(73);
        let a = filesystem(tmp.path(), &opts).await;
        let b = filesystem(tmp.path(), &opts).await;
        let ctl = Ctl::default();
        let (first, second) = tokio::join!(a.key_rotate(&ctl), b.key_rotate(&ctl));
        let mut epochs = [first.unwrap(), second.unwrap()];
        epochs.sort();
        assert_eq!(epochs, [2, 3]);
        let guard = a.key_guard(LockOperation::KeyRotate, &ctl).await.unwrap();
        let (previous, _) = marker(&a.store).await.unwrap();
        a.store
            .delete(&layout::lock_key(guard.lease.id().unwrap()))
            .await
            .unwrap();
        assert!(
            publish(&a, &guard, &previous, &previous, &ctl)
                .await
                .unwrap_err()
                .to_string()
                .contains("lease was lost")
        );
        assert_eq!(marker(&a.store).await.unwrap().0, previous);
        guard.release().await.unwrap();
        drop((a, b));
        assert_eq!(
            filesystem(tmp.path(), &opts)
                .await
                .security()
                .await
                .unwrap()
                .epoch(),
            Some(3)
        );
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_retargeted_alias_never_publishes_into_another_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = super::super::tests::options(74);
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        let a = filesystem(&first, &opts).await;
        let b = filesystem(&second, &opts).await;
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let opened = filesystem(&alias, &opts).await;
        let before_a = marker(&a.store).await.unwrap().0;
        let before_b = marker(&b.store).await.unwrap().0;
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&second, &alias).unwrap();
        assert!(
            opened
                .key_rotate(&Ctl::default())
                .await
                .unwrap_err()
                .to_string()
                .contains("root changed")
        );
        assert_eq!(marker(&a.store).await.unwrap().0, before_a);
        assert_eq!(marker(&b.store).await.unwrap().0, before_b);
        assert!(!second.join("sparkles-key-management.lock").exists());
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn failed_key_management_releases_native_lease_before_return() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = super::super::tests::options(75);
        let repo = filesystem(tmp.path(), &opts).await;
        let slot = repo.key_list().await.unwrap()[0].id;
        assert_eq!(
            repo.key_remove(slot, &Ctl::default())
                .await
                .unwrap_err()
                .code(),
            Code::LastKeySlot
        );
        assert!(
            lock::list_locks(repo.store.as_ref())
                .await
                .unwrap()
                .is_empty()
        );
        let different =
            slots::LocalKey::new(&opts.keys[0].label, slots::LocalKeySource::File, [76; 32])
                .unwrap();
        assert!(repo.key_add(&different, &Ctl::default()).await.is_err());
        assert!(
            lock::list_locks(repo.store.as_ref())
                .await
                .unwrap()
                .is_empty()
        );
        repo.store
            .put(&Key::from(ROTATION), PutPayload::from_static(b"{malformed"))
            .await
            .unwrap();
        assert!(repo.key_rotate(&Ctl::default()).await.is_err());
        assert!(
            lock::list_locks(repo.store.as_ref())
                .await
                .unwrap()
                .is_empty()
        );
        repo.store.delete(&Key::from(ROTATION)).await.unwrap();
        assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_lease_loss_or_expiry_during_preparation_refuses_publication() {
        for expired in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let opts = super::super::tests::options(78);
            let repo = Arc::new(filesystem(tmp.path(), &opts).await);
            let previous = marker(&repo.store).await.unwrap().0;
            let (entered, release) = local::pause(tmp.path());
            let task_repo = repo.clone();
            let task = tokio::spawn(async move { task_repo.key_rotate(&Ctl::default()).await });
            tokio::time::timeout(std::time::Duration::from_secs(3), entered)
                .await
                .unwrap()
                .unwrap();
            let leases = std::fs::read_dir(tmp.path().join("locks"))
                .unwrap()
                .collect::<std::io::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(leases.len(), 1);
            if expired {
                std::fs::File::open(leases[0].path())
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(
                        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
                    ))
                    .unwrap();
            } else {
                std::fs::remove_file(leases[0].path()).unwrap();
            }
            release.send(()).unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains(if expired {
                    "lease expired"
                } else {
                    "lease was lost"
                }),
                "{error}"
            );
            assert_eq!(marker(&repo.store).await.unwrap().0, previous);
            assert!(repo.store.head(&Key::from(ROTATION)).await.is_ok());
            assert_eq!(
                std::fs::read_dir(tmp.path().join("locks")).unwrap().count(),
                0
            );
            assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
        }
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn dropped_filesystem_preparation_only_cleans_staging_and_retains_lock_until_done() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = super::super::tests::options(79);
        let repo = Arc::new(filesystem(tmp.path(), &opts).await);
        let previous = marker(&repo.store).await.unwrap().0;
        let (entered, release) = local::pause(tmp.path());
        let task_repo = repo.clone();
        let task = tokio::spawn(async move { task_repo.key_rotate(&Ctl::default()).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(tmp.path().join("sparkles-key-management.lock"))
            .unwrap();
        assert!(matches!(
            file.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let staging = std::fs::read_dir(tmp.path()).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".sparkles-marker-")
                });
                if !staging && file.try_lock().is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(file);
        assert_eq!(marker(&repo.store).await.unwrap().0, previous);
        assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn canceled_or_dropped_directory_sync_retains_actual_lease_until_durability() {
        for aborted in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let opts = super::super::tests::options(80);
            let mut opened = filesystem(tmp.path(), &opts).await;
            opened.env.lock_wait = std::time::Duration::from_millis(25);
            let repo = Arc::new(opened);
            let (entered, release) = local::pause_sync(tmp.path());
            let ctl = Ctl::default();
            let cancel = ctl.cancel.clone();
            let task_repo = repo.clone();
            let mut task = Some(tokio::spawn(
                async move { task_repo.key_rotate(&ctl).await },
            ));
            tokio::time::timeout(std::time::Duration::from_secs(3), entered)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(repo.security().await.unwrap().epoch(), Some(2));
            if aborted {
                let running = task.take().unwrap();
                running.abort();
                assert!(running.await.unwrap_err().is_cancelled());
            } else {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                assert!(
                    tokio::time::timeout(
                        std::time::Duration::from_millis(75),
                        task.as_mut().unwrap()
                    )
                    .await
                    .is_err()
                );
            }
            assert!(repo.store.head(&Key::from(ROTATION)).await.is_ok());
            // Ordinary repository operations use the actual object lease, not the
            // private key-management advisory lock. Both remain held during sync.
            for kind in [LockKind::Shared, LockKind::Exclusive] {
                assert_eq!(
                    lock::acquire(&repo, kind, LockOperation::Verify, &Ctl::default())
                        .await
                        .unwrap_err()
                        .code(),
                    Code::RepositoryLocked
                );
            }
            assert_eq!(
                std::fs::read_dir(tmp.path().join("locks")).unwrap().count(),
                1
            );
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(tmp.path().join("sparkles-key-management.lock"))
                .unwrap();
            assert!(matches!(
                file.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            release.send(()).unwrap();
            if let Some(task) = task {
                assert!(
                    tokio::time::timeout(std::time::Duration::from_secs(3), task)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap_err()
                        .is_cancelled()
                );
            }
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if std::fs::read_dir(tmp.path().join("locks")).unwrap().count() == 0
                        && file.try_lock().is_ok()
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            drop(file);
            assert!(repo.store.head(&Key::from(ROTATION)).await.is_ok());
            assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
            assert!(repo.store.head(&Key::from(ROTATION)).await.is_err());
        }
    }
    #[cfg(all(feature = "fs", any(target_os = "linux", target_os = "android")))]
    #[tokio::test]
    async fn filesystem_recovery_retries_failed_directory_sync_before_removing_intent() {
        for retiring in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let opts = super::super::tests::options(83);
            let repo = Arc::new(filesystem(tmp.path(), &opts).await);
            if retiring {
                repo.key_rotate(&Ctl::default()).await.unwrap();
            }
            let old_slots = repository::read_slots(&repo.store)
                .await
                .unwrap()
                .into_iter()
                .filter(|s| s.epoch == 1)
                .map(|s| s.id)
                .collect::<Vec<_>>();
            let intent = Key::from(if retiring { RETIREMENT } else { ROTATION });
            let (entered, release) = local::pause_sync_failure(tmp.path());
            let task_repo = repo.clone();
            let task = tokio::spawn(async move {
                if retiring {
                    task_repo.key_retire(1, &Ctl::default()).await.map(|()| 2)
                } else {
                    task_repo.key_rotate(&Ctl::default()).await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), entered)
                .await
                .unwrap()
                .unwrap();
            release.send(()).unwrap();
            assert!(
                task.await
                    .unwrap()
                    .unwrap_err()
                    .to_string()
                    .contains("directory sync test failure")
            );
            assert_eq!(repo.security().await.unwrap().epoch(), Some(2));
            assert!(repo.store.head(&intent).await.is_ok());
            let (entered, release) = local::pause_sync(tmp.path());
            let task_repo = repo.clone();
            let retry = tokio::spawn(async move {
                if retiring {
                    task_repo
                        .key_add(
                            &slots::LocalKey::new(
                                "after retirement",
                                slots::LocalKeySource::File,
                                [84; 32],
                            )
                            .unwrap(),
                            &Ctl::default(),
                        )
                        .await
                        .map(|_| 2)
                } else {
                    task_repo.key_rotate(&Ctl::default()).await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), entered)
                .await
                .unwrap()
                .unwrap();
            assert!(repo.store.head(&intent).await.is_ok());
            for id in old_slots {
                assert!(repo.store.head(&repository::slot_key(id)).await.is_ok());
            }
            release.send(()).unwrap();
            assert_eq!(retry.await.unwrap().unwrap(), 2);
            assert!(repo.store.head(&intent).await.is_err());
            if retiring {
                assert!(
                    repository::read_slots(&repo.store)
                        .await
                        .unwrap()
                        .iter()
                        .all(|s| s.epoch == 2)
                );
            }
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn injected_filesystem_config_keeps_object_store_cas_and_never_touches_path() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("must-stay-absent");
        let store = Arc::new(InMemory::new());
        let opts = super::super::tests::options(77);
        let repo = Repository::open_encrypted(
            &crate::RepoConfig {
                name: "injected".into(),
                kind: crate::RepoType::Fs,
                path: Some(absent.to_str().unwrap().into()),
                conditional_writes: true,
                ..Default::default()
            },
            &crate::OpenEnv {
                store: Some(store),
                ..Default::default()
            },
            &opts,
        )
        .await
        .unwrap();
        assert!(repo.native_root.is_none());
        assert_eq!(repo.key_rotate(&Ctl::default()).await.unwrap(), 2);
        repo.key_retire(1, &Ctl::default()).await.unwrap();
        assert!(!absent.exists());
    }
    #[tokio::test]
    async fn passphrase_add_resumes_partial_publication_and_recovery_opens_every_epoch() {
        let store = Arc::new(InMemory::new());
        let opts = super::super::tests::options(55);
        let repo = super::super::tests::open(store.clone(), None, &opts).await;
        repo.key_rotate(&Ctl::default()).await.unwrap();
        let phrase =
            Passphrase::new("offline phrase", b"exact recovery phrase\n".to_vec()).unwrap();
        let snapshot = repo.security().await.unwrap().sealed.unwrap();
        let partial = Slot::passphrase(snapshot.key(1).unwrap(), &phrase).unwrap();
        repository::put_slot(&repo.store, &partial).await.unwrap();
        let added = repo
            .key_add_passphrase(&phrase, &Ctl::default())
            .await
            .unwrap();
        assert_eq!(added.len(), 2);
        assert!(added.iter().any(|slot| slot.id == partial.id));
        assert_eq!(
            repo.key_add_passphrase(&phrase, &Ctl::default())
                .await
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            added.iter().map(|s| s.id).collect::<Vec<_>>()
        );
        let wrong = Passphrase::new("offline phrase", b"wrong recovery phrase".to_vec()).unwrap();
        assert!(
            repo.key_add_passphrase(&wrong, &Ctl::default())
                .await
                .is_err()
        );
        assert_eq!(repo.key_list().await.unwrap().len(), 4);
        let recovered = super::super::tests::open(
            store,
            None,
            &EncryptionOptions {
                passphrases: vec![phrase],
                single_key_ok: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            recovered
                .security()
                .await
                .unwrap()
                .sealed
                .unwrap()
                .keys
                .len(),
            2
        );
    }
    #[tokio::test]
    async fn retirement_preserves_offline_recovery_keys_until_their_epoch_is_removed() {
        let store = Arc::new(InMemory::new());
        let opts = super::super::tests::options(51);
        let repo = super::super::tests::open(store.clone(), None, &opts).await;
        let recovery =
            slots::LocalKey::new("offline", slots::LocalKeySource::File, [52; 32]).unwrap();
        repo.key_add(&recovery, &Ctl::default()).await.unwrap();
        // This repository handle has only the online input; the recovery input is absent.
        assert_eq!(repo.key_list().await.unwrap().len(), 2);
        repo.key_rotate(&Ctl::default()).await.unwrap();
        repo.key_retire(1, &Ctl::default()).await.unwrap();
        let remaining = repo.key_list().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].epoch, 2);
    }

    #[tokio::test]
    async fn malformed_published_retirement_validates_every_target_before_deleting_any() {
        let store = Arc::new(InMemory::new());
        let opts = super::super::tests::options(53);
        let repo = super::super::tests::open(store.clone(), None, &opts).await;
        repo.key_rotate(&Ctl::default()).await.unwrap();
        let guard = repo
            .key_guard(LockOperation::KeyRotate, &Ctl::default())
            .await
            .unwrap();
        let (previous, _) = marker(&repo.store).await.unwrap();
        let mut next = previous.clone();
        let mut descriptor = Descriptor::parse(previous.encryption.as_ref().unwrap()).unwrap();
        descriptor.epochs.retain(|e| e.epoch != 1);
        next.encryption = Some(serde_json::to_value(descriptor).unwrap());
        let mut slots = repository::read_slots(&repo.store).await.unwrap();
        slots.sort_by_key(|slot| slot.epoch);
        let intent = Retirement {
            previous,
            next,
            epoch: 1,
            // A corrupt intent names the retired slot first, then an active one.
            slots: slots.iter().map(|slot| slot.id).collect(),
        };
        intent.check(repo.id()).unwrap();
        repo.store
            .put(
                &Key::from(RETIREMENT),
                PutPayload::from(serde_json::to_vec(&intent).unwrap()),
            )
            .await
            .unwrap();
        publish(
            &repo,
            &guard,
            &intent.previous,
            &intent.next,
            &Ctl::default(),
        )
        .await
        .unwrap();
        guard.release().await.unwrap();
        assert!(
            repo.key_add(
                &slots::LocalKey::new("added", slots::LocalKeySource::Env, [54; 32]).unwrap(),
                &Ctl::default(),
            )
            .await
            .is_err()
        );
        for slot in slots {
            assert!(store.head(&repository::slot_key(slot.id)).await.is_ok());
        }
        assert!(store.head(&Key::from(RETIREMENT)).await.is_ok());
    }

    #[tokio::test]
    async fn interrupted_rotation_preserves_old_readers_and_resumes_before_or_after_marker() {
        for boundary in 0..=3 {
            let store = Arc::new(InMemory::new());
            let mut opts = super::super::tests::options(40);
            opts.keys.push(
                slots::LocalKey::new("recovery", slots::LocalKeySource::File, [41; 32]).unwrap(),
            );
            let repo = super::super::tests::open(store.clone(), None, &opts).await;
            let guard = repo
                .key_guard(LockOperation::KeyRotate, &Ctl::default())
                .await
                .unwrap();
            let (previous, _) = marker(&repo.store).await.unwrap();
            let mut next = previous.clone();
            let mut descriptor = Descriptor::parse(previous.encryption.as_ref().unwrap()).unwrap();
            descriptor.epochs[0].state = "retired".into();
            descriptor.epochs.push(Epoch {
                epoch: 2,
                state: "active".into(),
                created: crate::now_rfc3339(),
            });
            next.encryption = Some(serde_json::to_value(descriptor).unwrap());
            let keys = EpochKeys::new(
                repo.id(),
                2,
                Zeroizing::new(primitive::random::<32>().unwrap()),
            )
            .unwrap();
            let slots = opts
                .keys
                .iter()
                .map(|key| Slot::local(&keys, key).unwrap())
                .collect::<Vec<_>>();
            let intent = Rotation {
                format: 1,
                kind: "sparkles-repo-rotation".into(),
                previous,
                next: next.clone(),
                slots,
            };
            repo.store
                .put(
                    &Key::from(ROTATION),
                    PutPayload::from(serde_json::to_vec(&intent).unwrap()),
                )
                .await
                .unwrap();
            for slot in intent.slots.iter().take(boundary.min(2)) {
                repository::put_slot(&repo.store, slot).await.unwrap();
            }
            if boundary == 3 {
                publish(
                    &repo,
                    &guard,
                    &intent.previous,
                    &intent.next,
                    &Ctl::default(),
                )
                .await
                .unwrap();
            }
            guard.release().await.unwrap();
            let reopened = super::super::tests::open(store.clone(), None, &opts).await;
            assert_eq!(
                reopened.security().await.unwrap().epoch(),
                Some(if boundary == 3 { 2 } else { 1 })
            );
            let canceled = Ctl::default();
            canceled
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(
                reopened
                    .key_rotate(&canceled)
                    .await
                    .unwrap_err()
                    .is_cancelled()
            );
            assert_eq!(
                reopened.security().await.unwrap().epoch(),
                Some(if boundary == 3 { 2 } else { 1 })
            );
            assert_eq!(reopened.key_rotate(&Ctl::default()).await.unwrap(), 2);
            assert_eq!(reopened.security().await.unwrap().epoch(), Some(2));
            assert!(store.head(&Key::from(ROTATION)).await.is_err());
            assert_eq!(reopened.key_list().await.unwrap().len(), 4);
        }
    }
    #[tokio::test]
    async fn interrupted_retirement_before_and_after_marker_keeps_repository_readable() {
        for published in [false, true] {
            let store = Arc::new(InMemory::new());
            let opts = super::super::tests::options(43);
            let repo = super::super::tests::open(store.clone(), None, &opts).await;
            repo.key_rotate(&Ctl::default()).await.unwrap();
            let guard = repo
                .key_guard(LockOperation::KeyRotate, &Ctl::default())
                .await
                .unwrap();
            let (previous, _) = marker(&repo.store).await.unwrap();
            let mut next = previous.clone();
            let mut descriptor = Descriptor::parse(previous.encryption.as_ref().unwrap()).unwrap();
            descriptor.epochs.retain(|e| e.epoch != 1);
            next.encryption = Some(serde_json::to_value(descriptor).unwrap());
            let slots = repository::read_slots(&repo.store)
                .await
                .unwrap()
                .into_iter()
                .filter(|s| s.epoch == 1)
                .map(|s| s.id)
                .collect();
            let intent = Retirement {
                previous,
                next,
                epoch: 1,
                slots,
            };
            repo.store
                .put(
                    &Key::from(RETIREMENT),
                    PutPayload::from(serde_json::to_vec(&intent).unwrap()),
                )
                .await
                .unwrap();
            if published {
                publish(
                    &repo,
                    &guard,
                    &intent.previous,
                    &intent.next,
                    &Ctl::default(),
                )
                .await
                .unwrap();
            }
            guard.release().await.unwrap();
            let reopened = super::super::tests::open(store.clone(), None, &opts).await;
            assert_eq!(reopened.security().await.unwrap().epoch(), Some(2));
            if !published {
                reopened.key_retire(1, &Ctl::default()).await.unwrap();
            } else {
                reopened
                    .key_add(
                        &slots::LocalKey::new("added", slots::LocalKeySource::Env, [44; 32])
                            .unwrap(),
                        &Ctl::default(),
                    )
                    .await
                    .unwrap();
            }
            assert!(
                reopened
                    .key_list()
                    .await
                    .unwrap()
                    .iter()
                    .all(|s| s.epoch == 2)
            );
            assert!(store.head(&Key::from(RETIREMENT)).await.is_err());
        }
    }
}
