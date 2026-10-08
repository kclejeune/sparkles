//! One coherent repository encryption snapshot per operation. Plain repositories
//! retain their existing object encoding and SHA256 IDs.
use crate::{Repository, Result, blob};
use bytes::Bytes;
use object_store::PutPayload;
#[cfg(feature = "encryption")]
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) enum KeyOptions {
    #[default]
    None,
    #[cfg(feature = "encryption")]
    Local(Arc<crate::crypto::KeyState>),
}

#[derive(Clone, Default)]
pub(crate) struct Security {
    #[cfg(feature = "encryption")]
    pub sealed: Option<Arc<crate::crypto::Snapshot>>,
}

pub(crate) enum IdHasher {
    Plain(blob::Hasher),
    #[cfg(feature = "encryption")]
    Sealed(Box<aws_lc_rs::hmac::Context>),
}
impl IdHasher {
    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Plain(h) => h.update(bytes),
            #[cfg(feature = "encryption")]
            Self::Sealed(h) => h.update(bytes),
        }
    }
    pub fn finish(self) -> String {
        match self {
            Self::Plain(h) => h.finish(),
            #[cfg(feature = "encryption")]
            Self::Sealed(h) => blob::hex((*h).sign().as_ref()),
        }
    }
}
impl Security {
    pub fn encrypted(&self) -> bool {
        #[cfg(feature = "encryption")]
        {
            self.sealed.is_some()
        }
        #[cfg(not(feature = "encryption"))]
        {
            false
        }
    }
    pub fn epoch(&self) -> Option<u32> {
        #[cfg(feature = "encryption")]
        {
            self.sealed.as_ref().map(|keys| keys.descriptor.active())
        }
        #[cfg(not(feature = "encryption"))]
        {
            None
        }
    }
    pub fn id_hasher(&self) -> IdHasher {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.sealed {
            return IdHasher::Sealed(Box::new(aws_lc_rs::hmac::Context::with_key(
                &aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, keys.active().id.as_ref()),
            )));
        }
        IdHasher::Plain(blob::Hasher::new())
    }
    pub fn blob_id(&self, plain: &[u8]) -> String {
        let mut hash = self.id_hasher();
        hash.update(plain);
        hash.finish()
    }
    pub fn encode(&self, plain: Bytes) -> Result<PutPayload> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.sealed {
            return Ok(PutPayload::from(
                keys.active().seal_blob(&plain, true)?.bytes,
            ));
        }
        Ok(crate::create::encode(plain))
    }
    pub fn decode(&self, stored: &[u8], id: &str, size: u64) -> Result<Vec<u8>> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.sealed {
            return keys.open_blob(stored, id, size);
        }
        blob::decode(stored, id, size).map_err(Into::into)
    }
    pub fn blob_limit(&self, size: u64) -> Result<u64> {
        #[cfg(feature = "encryption")]
        if self.encrypted() {
            return crate::crypto::objects::stored_blob_limit(size);
        }
        Ok(blob::HEADER_LEN as u64 + size + size / 64 + 4096)
    }
    pub fn plausible_size(&self, plain: u64, stored: u64) -> bool {
        if self.encrypted() {
            self.blob_limit(plain)
                .is_ok_and(|limit| stored >= 48 && stored <= limit)
        } else {
            crate::verify::plausible_size(plain, stored)
        }
    }
    pub fn manifest_limit(&self) -> u64 {
        #[cfg(feature = "encryption")]
        if self.encrypted() {
            return crate::crypto::objects::MAX_STORED_MANIFEST_BYTES;
        }
        crate::layout::MAX_MANIFEST_BYTES
    }
    pub fn seal_manifest(&self, name: &str, plain: Vec<u8>) -> Result<Vec<u8>> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.sealed {
            return keys.active().seal_manifest(name, &plain);
        }
        let _ = name;
        Ok(plain)
    }
    pub fn open_manifest(&self, name: &str, stored: &[u8]) -> Result<crate::Manifest> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.sealed {
            let epoch = crate::crypto::objects::Envelope::parse(stored)?.epoch;
            let key = keys.key(epoch)?;
            let plain = key.open_manifest(name, stored)?;
            let manifest = crate::manifest::parse(&plain).map_err(|_| {
                crate::BackupError::invalid_backup("manifest", "decrypted manifest does not parse")
            })?;
            if manifest.name != name
                || manifest.repository_id != key.repository
                || manifest
                    .encryption
                    .as_ref()
                    .and_then(|e| e.get("epoch"))
                    .and_then(|e| e.as_u64())
                    != Some(epoch as u64)
            {
                return Err(crate::BackupError::invalid_backup(
                    "encryption",
                    "manifest identity or epoch does not match envelope",
                ));
            }
            return Ok(manifest);
        }
        let _ = name;
        let m = crate::manifest::parse(stored)?;
        if m.encryption.as_ref().is_some_and(|e| !e.is_null()) {
            return Err(crate::BackupError::invalid_backup(
                "encryption",
                "encrypted metadata in a plaintext repository",
            ));
        }
        Ok(m)
    }
}

impl Repository {
    pub(crate) async fn security(&self) -> Result<Security> {
        let _ = &self.key_options;
        #[cfg(feature = "encryption")]
        if let KeyOptions::Local(keys) = &self.key_options {
            return Ok(Security {
                sealed: Some(
                    keys.snapshot(&self.store, self.marker.id, self.cache.dir.as_deref())
                        .await?,
                ),
            });
        }
        Ok(Security::default())
    }
}
