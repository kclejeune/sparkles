use super::{objects::EpochKeys, primitive};
use crate::{BackupError, Code, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_SLOTS: usize = 32;
pub const MAX_SLOT_BYTES: usize = 16384;
/// The slot format written for new slots. Format 1 slots remain readable.
pub const SLOT_FORMAT: u32 = 2;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LocalKeySource {
    File,
    Env,
    Credential,
    Command,
}
impl LocalKeySource {
    fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Env => "env",
            Self::Credential => "credential",
            Self::Command => "command",
        }
    }
}

/// An explicit resolved KEK; provider lookup is never inferred from HTTP input.
#[derive(Clone)]
pub struct LocalKey {
    pub label: String,
    pub source: LocalKeySource,
    pub(super) secret: std::sync::Arc<super::memory::LockedKey>,
}
impl std::fmt::Debug for LocalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalKey")
            .field("label", &self.label)
            .field("source", &self.source)
            .field("secret", &"[redacted]")
            .finish()
    }
}
impl LocalKey {
    /// Pass a [`Zeroizing`] buffer to avoid leaving an unzeroized copy of the key
    /// with the caller. A plain array is accepted for compatibility and tests.
    pub fn new(
        label: impl Into<String>,
        source: LocalKeySource,
        key: impl Into<Zeroizing<[u8; 32]>>,
    ) -> Result<Self> {
        let key = key.into();
        let label = label.into();
        check_label(&label)?;
        Ok(Self {
            label,
            source,
            secret: std::sync::Arc::new(super::memory::LockedKey::new(key)?),
        })
    }
}

#[derive(Clone)]
pub struct Passphrase {
    pub label: String,
    secret: std::sync::Arc<super::memory::LockedKey>,
}
impl std::fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passphrase")
            .field("label", &self.label)
            .field("secret", &"[redacted]")
            .finish()
    }
}
impl Passphrase {
    pub fn new(label: impl Into<String>, passphrase: Vec<u8>) -> Result<Self> {
        let secret = Zeroizing::new(passphrase);
        let label = label.into();
        check_label(&label)?;
        if secret.is_empty() || secret.len() > 4096 {
            return Err(config("passphrase length must be 1..4096 bytes"));
        }
        Ok(Self {
            label,
            secret: std::sync::Arc::new(super::memory::LockedKey::from_bytes(secret)?),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct EncryptionOptions {
    pub keys: Vec<LocalKey>,
    pub passphrases: Vec<Passphrase>,
    /// Explicitly accept initializing with just one recovery slot.
    pub single_key_ok: bool,
}
impl EncryptionOptions {
    pub fn check(&self, initializing: bool) -> Result<()> {
        let total = self.keys.len() + self.passphrases.len();
        if total == 0 {
            return Err(required());
        }
        if total > 8 {
            return Err(config("at most 8 explicit key inputs are supported"));
        }
        let mut labels = HashSet::new();
        for label in self
            .keys
            .iter()
            .map(|k| &k.label)
            .chain(self.passphrases.iter().map(|k| &k.label))
        {
            check_label(label)?;
            if !labels.insert(label) {
                return Err(config("key labels must be unique"));
            }
        }
        if initializing && total == 1 && !self.single_key_ok {
            return Err(config("one slot requires explicit single_key_ok"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Descriptor {
    pub scheme: String,
    pub cipher: String,
    pub ids: String,
    pub kdf: String,
    pub epochs: Vec<Epoch>,
    pub padding: String,
    pub write_only: bool,
    /// HMAC-SHA256 over the repository id and every other descriptor field, under a
    /// subkey of the active epoch's master key. Descriptors written before this field
    /// existed have none. They are accepted until this host has seen an authenticated
    /// descriptor for the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Epoch {
    pub epoch: u32,
    pub state: String,
    pub created: String,
}
impl Descriptor {
    pub fn new() -> Self {
        Self {
            scheme: "sparkles-repo-v1".into(),
            cipher: "aes-256-gcm".into(),
            ids: "hmac-sha256".into(),
            kdf: "hkdf-sha256".into(),
            epochs: vec![Epoch {
                epoch: 1,
                state: "active".into(),
                created: crate::now_rfc3339(),
            }],
            padding: "padme".into(),
            write_only: false,
            mac: None,
        }
    }
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let value: Self = serde_json::from_value(value.clone()).map_err(|_| incompatible())?;
        let mut ids = HashSet::new();
        if value.scheme != "sparkles-repo-v1"
            || value.cipher != "aes-256-gcm"
            || value.ids != "hmac-sha256"
            || value.kdf != "hkdf-sha256"
            || value.padding != "padme"
            || value.write_only
            || value.epochs.is_empty()
            || value.epochs.len() > MAX_SLOTS
            || value.epochs.iter().filter(|e| e.state == "active").count() != 1
            || value.epochs.iter().any(|e| {
                e.epoch == 0
                    || !ids.insert(e.epoch)
                    || !matches!(e.state.as_str(), "active" | "retired")
                    || chrono::DateTime::parse_from_rfc3339(&e.created).is_err()
            })
            || value
                .mac
                .as_ref()
                .is_some_and(|mac| mac.len() != 64 || !is_lower_hex(mac))
        {
            return Err(incompatible());
        }
        // Rotation always activates a new highest epoch and retirement never removes
        // the active one, so an older epoch can never legitimately become active again.
        if value.active() != value.max_epoch() {
            return Err(tampered("the active key epoch is not the newest epoch"));
        }
        Ok(value)
    }
    pub fn active(&self) -> u32 {
        self.epochs
            .iter()
            .find(|e| e.state == "active")
            .expect("descriptor checked")
            .epoch
    }
    pub fn max_epoch(&self) -> u32 {
        self.epochs.iter().map(|e| e.epoch).max().unwrap_or(0)
    }
    /// The same descriptor without its authentication tag.
    pub fn unsigned(&self) -> Self {
        Self {
            mac: None,
            ..self.clone()
        }
    }
    fn signed_bytes(&self, repository: Uuid) -> Vec<u8> {
        let mut bytes = DESCRIPTOR_INFO.to_vec();
        bytes.push(0);
        bytes.extend_from_slice(repository.as_bytes());
        bytes.extend_from_slice(&serde_json::to_vec(&self.unsigned()).expect("serializes"));
        bytes
    }
    fn mac_key(keys: &EpochKeys) -> Result<Zeroizing<[u8; 32]>> {
        primitive::derive(keys.master.as_ref(), &[], DESCRIPTOR_INFO)
    }
    /// Authenticate the descriptor under the active epoch's master key.
    pub fn sign(&mut self, keys: &EpochKeys) -> Result<()> {
        if keys.epoch != self.active() {
            return Err(config("descriptor must be signed by its active epoch"));
        }
        let tag = primitive::mac(
            Self::mac_key(keys)?.as_ref(),
            &self.signed_bytes(keys.repository),
        );
        self.mac = Some(crate::blob::hex(&tag));
        Ok(())
    }
    /// Check the tag, when present, under the active epoch's master key. A missing
    /// tag marks a legacy descriptor, and the caller decides whether to accept it.
    pub fn verify(&self, keys: &EpochKeys) -> Result<()> {
        let Some(mac) = &self.mac else {
            return Ok(());
        };
        let tag = mac
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(incompatible)?;
        if keys.epoch != self.active()
            || !primitive::mac_verify(
                Self::mac_key(keys)?.as_ref(),
                &self.signed_bytes(keys.repository),
                &tag,
            )
        {
            return Err(tampered(
                "repository key epoch metadata failed authentication",
            ));
        }
        Ok(())
    }
}

const DESCRIPTOR_INFO: &[u8] = b"sparkles/f07/repo/descriptor";

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Argon {
    pub algorithm: String,
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
    pub salt: String,
}
impl Argon {
    fn new() -> Result<Self> {
        Ok(Self {
            algorithm: "argon2id".into(),
            memory_kib: 65536,
            passes: 3,
            lanes: 4,
            salt: STANDARD.encode(primitive::random::<16>()?),
        })
    }
    fn derive(&self, phrase: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
        // Validate BEFORE any expensive allocation. Only the scheme1 KDF profile is
        // accepted in this slice, so hostile metadata cannot amplify resource use.
        if self.algorithm != "argon2id"
            || self.memory_kib != 65536
            || self.passes != 3
            || self.lanes != 4
        {
            return Err(config("unsupported or excessive passphrase KDF parameters"));
        }
        let salt = super::objects::decode_bounded(&self.salt, 16)?;
        if salt.len() != 16 {
            return Err(config("invalid passphrase salt"));
        }
        let params = argon2::Params::new(self.memory_kib, self.passes, self.lanes, Some(32))
            .map_err(|_| config("invalid passphrase KDF parameters"))?;
        let mut key = Zeroizing::new([0; 32]);
        argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
            .hash_password_into(phrase, &salt, key.as_mut())
            .map_err(|_| config("passphrase KDF failed"))?;
        Ok(key)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Slot {
    pub kind: String,
    pub format: u32,
    pub id: Uuid,
    pub epoch: u32,
    pub label: String,
    pub created: String,
    pub source: String,
    pub kek_id: String,
    pub nonce: String,
    pub ct: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf: Option<Argon>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySlotSummary {
    pub id: Uuid,
    pub epoch: u32,
    pub label: String,
    pub created: String,
    pub source: String,
    pub kek_id: String,
}
impl Slot {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_SLOT_BYTES {
            return Err(config("key slot exceeds limit"));
        }
        let slot: Self =
            serde_json::from_slice(bytes).map_err(|_| config("invalid key slot metadata"))?;
        check_label(&slot.label)?;
        let (source, fingerprint) = slot
            .kek_id
            .split_once(':')
            .ok_or_else(|| config("invalid key fingerprint"))?;
        if slot.kind != "sparkles-key-slot"
            || !matches!(slot.format, 1 | SLOT_FORMAT)
            || slot.id.is_nil()
            || slot.epoch == 0
            || chrono::DateTime::parse_from_rfc3339(&slot.created).is_err()
            || !matches!(
                slot.source.as_str(),
                "file" | "env" | "credential" | "command" | "passphrase"
            )
            || source != slot.source
            || fingerprint.len() != 12
            || !fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || (slot.source == "passphrase") != slot.kdf.is_some()
        {
            return Err(config("unsupported key slot metadata"));
        }
        if super::objects::decode_bounded(&slot.nonce, 12)?.len() != 12
            || super::objects::decode_bounded(&slot.ct, 48)?.len() != 48
        {
            return Err(config("invalid wrapped key size"));
        }
        // Validate KDF profile before attempting supplied passphrases.
        if let Some(kdf) = &slot.kdf
            && (kdf.algorithm != "argon2id"
                || kdf.memory_kib != 65536
                || kdf.passes != 3
                || kdf.lanes != 4
                || super::objects::decode_bounded(&kdf.salt, 16)?.len() != 16)
        {
            return Err(config("unsupported or excessive passphrase KDF parameters"));
        }
        Ok(slot)
    }
    pub fn local(keys: &EpochKeys, key: &LocalKey) -> Result<Self> {
        Self::wrap(
            keys,
            &key.label,
            key.source.name(),
            key.secret.as_ref().as_ref(),
            None,
        )
    }
    pub fn passphrase(keys: &EpochKeys, key: &Passphrase) -> Result<Self> {
        let kdf = Argon::new()?;
        let kek = kdf.derive(key.secret.as_ref().as_ref())?;
        Self::wrap(keys, &key.label, "passphrase", kek.as_ref(), Some(kdf))
    }
    fn wrap(
        keys: &EpochKeys,
        label: &str,
        source: &str,
        kek: &[u8],
        kdf: Option<Argon>,
    ) -> Result<Self> {
        check_label(label)?;
        let nonce = primitive::random::<12>()?;
        let mut slot = Self {
            kind: "sparkles-key-slot".into(),
            format: SLOT_FORMAT,
            id: Uuid::new_v4(),
            epoch: keys.epoch,
            label: label.into(),
            created: crate::now_rfc3339(),
            source: source.into(),
            kek_id: fingerprint(source, kek),
            nonce: STANDARD.encode(nonce),
            ct: String::new(),
            kdf,
        };
        let ct = primitive::seal(kek, nonce, &slot.aad(keys.repository), keys.master.as_ref())?;
        slot.ct = STANDARD.encode(ct);
        Ok(slot)
    }
    /// Associated data of the wrap. Format 1 binds the repository, epoch and slot id.
    /// Format 2 also binds the label, creation time, source, `kekId` and KDF
    /// parameters, so they cannot be edited without the slot failing to open.
    fn aad(&self, repository: Uuid) -> Vec<u8> {
        let mut bytes = repository.as_bytes().to_vec();
        bytes.extend_from_slice(&self.epoch.to_le_bytes());
        bytes.extend_from_slice(self.id.as_bytes());
        if self.format >= SLOT_FORMAT {
            let kdf = self
                .kdf
                .as_ref()
                .map(|kdf| serde_json::to_vec(kdf).expect("serializes"))
                .unwrap_or_default();
            bytes.extend_from_slice(b"sparkles/f07/slot-metadata");
            for field in [
                self.label.as_bytes(),
                self.created.as_bytes(),
                self.source.as_bytes(),
                self.kek_id.as_bytes(),
                &kdf,
            ] {
                bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
                bytes.extend_from_slice(field);
            }
        }
        bytes
    }
    pub fn is_passphrase(&self) -> bool {
        self.kdf.is_some()
    }
    pub fn open(&self, repository: Uuid, options: &EncryptionOptions) -> Result<Option<EpochKeys>> {
        let mut keks = Vec::new();
        if let Some(kdf) = &self.kdf {
            // Explicit passphrase labels identify their slot, bounding expensive KDF
            // attempts and avoiding evaluation of unrelated untrusted slots.
            for input in &options.passphrases {
                if input.label == self.label {
                    keks.push(kdf.derive(input.secret.as_ref().as_ref())?);
                }
            }
        } else {
            for input in &options.keys {
                // Provider metadata records provenance, not permission to use
                // resolved bytes. Match fingerprints with the stored prefix below
                // so a file/env/credential migration preserves existing wraps.
                let mut kek = Zeroizing::new([0; 32]);
                kek.copy_from_slice(input.secret.as_ref().as_ref());
                keks.push(kek);
            }
        }
        for kek in keks {
            if fingerprint(&self.source, kek.as_ref()) != self.kek_id {
                continue;
            }
            let nonce = super::objects::decode_bounded(&self.nonce, 12)?;
            let nonce: [u8; 12] = nonce
                .try_into()
                .map_err(|_| config("invalid key slot nonce"))?;
            let ct = super::objects::decode_bounded(&self.ct, 48)?;
            let opened = primitive::open(kek.as_ref(), nonce, &self.aad(repository), &ct)?;
            if opened.len() != 32 {
                return Err(config("invalid repository master key"));
            }
            let mut master = Zeroizing::new([0; 32]);
            master.copy_from_slice(&opened);
            return Ok(Some(EpochKeys::new(repository, self.epoch, master)?));
        }
        Ok(None)
    }
    pub fn summary(&self) -> KeySlotSummary {
        KeySlotSummary {
            id: self.id,
            epoch: self.epoch,
            label: self.label.clone(),
            created: self.created.clone(),
            source: self.source.clone(),
            kek_id: self.kek_id.clone(),
        }
    }
}

fn fingerprint(source: &str, key: &[u8]) -> String {
    format!(
        "{source}:{}",
        crate::blob::hex(&primitive::mac(key, b"sparkles/f07/kek-id")[..6])
    )
}
fn check_label(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
        return Err(config("invalid key slot label"));
    }
    Ok(())
}
pub(crate) fn required() -> BackupError {
    BackupError::new(Code::RepositoryKeyRequired, "repository key is required")
}
pub(crate) fn config(message: &str) -> BackupError {
    BackupError::new(Code::InvalidConfig, message)
}
/// Repository key metadata that fails authentication or consistency checks.
pub(crate) fn tampered(message: &str) -> BackupError {
    BackupError::invalid_backup("encryption", message)
}
pub(crate) fn incompatible() -> BackupError {
    BackupError::new(
        Code::IncompatibleRepository,
        "unsupported encrypted repository scheme or metadata",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_slots_authenticate_repository_epoch_identity_and_hide_keys() {
        let repo = Uuid::new_v4();
        let master = EpochKeys::new(repo, 1, Zeroizing::new([5; 32])).unwrap();
        let key = LocalKey::new("ops", LocalKeySource::File, [9; 32]).unwrap();
        let slot = Slot::local(&master, &key).unwrap();
        let opts = EncryptionOptions {
            keys: vec![key.clone()],
            single_key_ok: true,
            ..Default::default()
        };
        assert_eq!(
            slot.open(repo, &opts).unwrap().unwrap().master.as_ref(),
            master.master.as_ref()
        );
        assert!(slot.open(Uuid::new_v4(), &opts).is_err());
        let mut bad = slot.clone();
        bad.epoch += 1;
        assert!(bad.open(repo, &opts).is_err());
        let bytes = serde_json::to_vec(&slot).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains(&"05".repeat(32)));
        assert!(!format!("{key:?}").contains(&"09".repeat(32)));
        let wrong = EncryptionOptions {
            keys: vec![LocalKey::new("wrong", LocalKeySource::File, [3; 32]).unwrap()],
            single_key_ok: true,
            ..Default::default()
        };
        assert!(slot.open(repo, &wrong).unwrap().is_none());
    }
    #[test]
    fn new_slots_bind_their_metadata_and_legacy_slots_still_open() {
        let repo = Uuid::new_v4();
        let master = EpochKeys::new(repo, 1, Zeroizing::new([6; 32])).unwrap();
        let key = LocalKey::new("ops", LocalKeySource::File, Zeroizing::new([8; 32])).unwrap();
        let opts = EncryptionOptions {
            keys: vec![key.clone()],
            single_key_ok: true,
            ..Default::default()
        };
        let slot = Slot::local(&master, &key).unwrap();
        assert_eq!(slot.format, SLOT_FORMAT);
        assert!(slot.open(repo, &opts).unwrap().is_some());
        for edit in [
            (|s: &mut Slot| s.label = "relabeled".into()) as fn(&mut Slot),
            |s| s.created = "2001-01-01T00:00:00Z".into(),
            |s| s.format = 1,
        ] {
            let mut bad = slot.clone();
            edit(&mut bad);
            assert!(bad.open(repo, &opts).is_err());
        }
        // A format 1 slot binds only the repository, epoch and slot id.
        let mut legacy = slot.clone();
        legacy.format = 1;
        let nonce: [u8; 12] = STANDARD.decode(&legacy.nonce).unwrap().try_into().unwrap();
        legacy.ct = STANDARD.encode(
            primitive::seal(&[8; 32], nonce, &legacy.aad(repo), master.master.as_ref()).unwrap(),
        );
        let legacy = Slot::parse(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(legacy.open(repo, &opts).unwrap().is_some());
    }
    #[test]
    fn descriptor_tags_bind_repository_and_epochs() {
        let repo = Uuid::new_v4();
        let keys = EpochKeys::new(repo, 1, Zeroizing::new([4; 32])).unwrap();
        let mut descriptor = Descriptor::new();
        descriptor.sign(&keys).unwrap();
        let parsed = Descriptor::parse(&serde_json::to_value(&descriptor).unwrap()).unwrap();
        parsed.verify(&keys).unwrap();
        let other = EpochKeys::new(Uuid::new_v4(), 1, Zeroizing::new([4; 32])).unwrap();
        assert!(parsed.verify(&other).is_err());
        let mut changed = parsed.clone();
        changed.epochs[0].created = "2001-01-01T00:00:00Z".into();
        assert!(changed.verify(&keys).is_err());
        let mut older_active = descriptor.unsigned();
        older_active.epochs[0].state = "retired".into();
        older_active.epochs.push(Epoch {
            epoch: 2,
            state: "active".into(),
            created: crate::now_rfc3339(),
        });
        assert!(Descriptor::parse(&serde_json::to_value(&older_active).unwrap()).is_ok());
        older_active.epochs[0].state = "active".into();
        older_active.epochs[1].state = "retired".into();
        assert!(Descriptor::parse(&serde_json::to_value(&older_active).unwrap()).is_err());
    }
    #[test]
    fn malicious_argon_parameters_are_rejected_before_derivation() {
        let kdf = Argon {
            algorithm: "argon2id".into(),
            memory_kib: u32::MAX,
            passes: u32::MAX,
            lanes: 4,
            salt: STANDARD.encode([0; 16]),
        };
        assert!(kdf.derive(b"passphrase").is_err());
    }
}
