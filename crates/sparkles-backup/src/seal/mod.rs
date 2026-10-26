//! Sealed values of a server's runtime secrets, one small value per file.
//!
//! This module is built with every feature set, so that a server seals its runtime
//! secrets whether or not it has the `encryption` feature of the repository engine. It
//! uses `aws-lc-rs`, which rustls already brings into the server's default build.
//!
//! A sealed value is one line of text: [`SEALED_SECRET_PREFIX`], the base64 of a fresh
//! 16-byte salt and the base64 of the AES-256-GCM ciphertext with its tag. The value
//! key is derived from the operator's key and the salt with HKDF-SHA256, so the zero
//! nonce is used once under each derived key, as for the repository objects. The
//! secret's name is part of the authenticated data, so a sealed file renamed to another
//! secret's name does not open.
//!
//! The operator's key is kept in a locked page that core dumps leave out where the
//! platform has one (Linux, Android and a prepared macOS process), and in a zeroizing heap buffer elsewhere or
//! when the page cannot be locked. [`SecretSealer::key_memory`] says which.

#[cfg_attr(not(feature = "encryption"), allow(dead_code))]
pub(crate) mod memory;
#[cfg_attr(not(feature = "encryption"), allow(dead_code))]
pub(crate) mod primitive;

use crate::{BackupError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use memory::LockedKey;
use std::sync::Arc;
use zeroize::Zeroizing;

/// The first bytes of a sealed secret, including its format version.
pub use sparkles_core::formats::SEALED_SECRET_PREFIX;

const INFO: &[u8] = b"sparkles/secrets/value";
const MAX_PLAIN: usize = 64 * 1024;

fn aad(name: &str) -> Vec<u8> {
    let mut out = b"sparkles/secrets/1\0".to_vec();
    out.extend_from_slice(name.as_bytes());
    out
}

pub(crate) fn invalid(message: &str) -> BackupError {
    BackupError::invalid_backup("encryption", message)
}

pub(crate) fn decode_bounded(value: &str, limit: usize) -> Result<Vec<u8>> {
    if value.len() > limit.saturating_add(2) / 3 * 4 {
        return Err(invalid("encoded value exceeds limit"));
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| invalid("invalid encoded value"))?;
    if bytes.len() > limit {
        return Err(invalid("decoded value exceeds limit"));
    }
    Ok(bytes)
}

/// Where a sealer keeps its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMemory {
    /// A locked page that core dumps leave out.
    Locked,
    /// A heap buffer that is zeroed when the sealer is dropped.
    Heap,
}

impl KeyMemory {
    /// `"locked"` or `"heap"`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyMemory::Locked => "locked",
            KeyMemory::Heap => "heap",
        }
    }
}

enum Key {
    Locked(Arc<LockedKey>),
    Heap(Arc<Zeroizing<[u8; 32]>>),
}

impl Key {
    fn bytes(&self) -> &[u8] {
        match self {
            Key::Locked(k) => k.as_ref().as_ref(),
            Key::Heap(k) => k.as_ref().as_ref(),
        }
    }
}

impl Clone for Key {
    fn clone(&self) -> Self {
        match self {
            Key::Locked(k) => Key::Locked(k.clone()),
            Key::Heap(k) => Key::Heap(k.clone()),
        }
    }
}

/// Seals and opens runtime secret values with one operator key.
#[derive(Clone)]
pub struct SecretSealer {
    key: Key,
}

impl std::fmt::Debug for SecretSealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretSealer([redacted])")
    }
}

impl SecretSealer {
    /// A sealer that uses the key of a repository key input, which stays in its
    /// protected memory.
    #[cfg(feature = "encryption")]
    pub fn new(key: &crate::crypto::LocalKey) -> Self {
        Self {
            key: Key::Locked(key.secret.clone()),
        }
    }

    /// A sealer with the 32-byte key `key`. The key moves to a locked page when the
    /// platform allows it, and stays in a zeroizing heap buffer otherwise.
    pub fn from_key(key: Zeroizing<[u8; 32]>) -> Self {
        match LockedKey::new(Zeroizing::new(*key)) {
            Ok(locked) => Self {
                key: Key::Locked(Arc::new(locked)),
            },
            Err(_) => Self::heap(key),
        }
    }

    /// A sealer whose key stays in a zeroizing heap buffer.
    fn heap(key: Zeroizing<[u8; 32]>) -> Self {
        Self {
            key: Key::Heap(Arc::new(key)),
        }
    }

    /// Where this sealer keeps its key.
    pub fn key_memory(&self) -> KeyMemory {
        match self.key {
            Key::Locked(_) => KeyMemory::Locked,
            Key::Heap(_) => KeyMemory::Heap,
        }
    }

    /// 32 random bytes for a new key.
    pub fn generate_key() -> Result<Zeroizing<[u8; 32]>> {
        primitive::random_key()
    }

    /// Whether `stored` is a sealed value rather than a plaintext one.
    pub fn is_sealed(stored: &[u8]) -> bool {
        stored.starts_with(sparkles_core::formats::SEALED_SECRET_FAMILY.as_bytes())
    }

    /// The sealed form of the value `plain` of secret `name`.
    pub fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        if plain.len() > MAX_PLAIN {
            return Err(invalid("secret value is too long"));
        }
        let salt = primitive::random::<16>()?;
        let key = primitive::derive(self.key.bytes(), &salt, INFO)?;
        let ct = primitive::seal(key.as_ref(), [0; 12], &aad(name), plain)?;
        let mut out = SEALED_SECRET_PREFIX.as_bytes().to_vec();
        out.extend_from_slice(STANDARD.encode(salt).as_bytes());
        out.push(b' ');
        out.extend_from_slice(STANDARD.encode(ct).as_bytes());
        out.push(b'\n');
        Ok(out)
    }

    /// The value of secret `name` from its sealed form. A different key, a changed
    /// file or a file that belongs to another name fails authentication.
    pub fn open(&self, name: &str, stored: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let version =
            sparkles_core::formats::secret_version(stored).map_err(|e| invalid(&e.to_string()))?;
        if version == 0 {
            return Err(invalid("not a sealed secret"));
        }
        let body = stored
            .strip_prefix(SEALED_SECRET_PREFIX.as_bytes())
            .ok_or_else(|| invalid("not a sealed secret"))?;
        let body = std::str::from_utf8(body).map_err(|_| invalid("not a sealed secret"))?;
        let mut parts = body.trim_end().split(' ');
        let (Some(salt), Some(ct), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid("malformed sealed secret"));
        };
        let salt = decode_bounded(salt, 16)?;
        if salt.len() != 16 {
            return Err(invalid("malformed sealed secret"));
        }
        let ct = decode_bounded(ct, MAX_PLAIN + primitive::TAG_BYTES)?;
        let key = primitive::derive(self.key.bytes(), &salt, INFO)?;
        primitive::open(key.as_ref(), [0; 12], &aad(name), &ct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_v1_fixture_still_opens() {
        // Generated by the preserved item68 writer, before the second remediation.
        let fixture = b"sparkles-sealed-secret/1 W1uXE5fUwNbkE0QWMW1HcQ== A+rY6ocMHQVlO12w4lkCDjRpW6mpTwAUwfT+SaZK+4U=\n";
        let s = SecretSealer::from_key(Zeroizing::new([7; 32]));
        assert_eq!(&*s.open("legacy", fixture).unwrap(), b"old-format-value");
        assert!(s.open("another-name", fixture).is_err());
    }

    #[test]
    fn sealed_family_detection_is_distinct_from_supported_decoding() {
        let s = SecretSealer::from_key(Zeroizing::new([7; 32]));
        for version in ["0", "2", "x", "4294967296", "", "+1", "01"] {
            let bytes = format!("sparkles-sealed-secret/{version} body");
            assert!(
                SecretSealer::is_sealed(bytes.as_bytes()),
                "family version {version}"
            );
            let e = s.open("legacy", bytes.as_bytes()).unwrap_err().to_string();
            assert!(e.contains("version"), "{e}");
            assert!(!e.contains("body"));
        }
        assert!(!SecretSealer::is_sealed(b"plaintext"));
        assert!(s.open("legacy", b"plaintext").is_err());
        assert!(
            s.open("legacy", b"sparkles-sealed-secret/1 broken")
                .is_err()
        );
    }

    fn check(s: &SecretSealer, other: &SecretSealer) {
        let sealed = s.seal("openai", b"sk-abc").unwrap();
        assert!(SecretSealer::is_sealed(&sealed));
        assert!(!sealed.windows(6).any(|w| w == b"sk-abc"));
        assert_eq!(&*s.open("openai", &sealed).unwrap(), b"sk-abc");
        // a fresh salt each time
        assert_ne!(sealed, s.seal("openai", b"sk-abc").unwrap());
        assert!(s.open("anthropic", &sealed).is_err());
        assert!(other.open("openai", &sealed).is_err());
        let mut changed = sealed.clone();
        let at = changed.len() - 4;
        changed[at] = if changed[at] == b'A' { b'B' } else { b'A' };
        assert!(s.open("openai", &changed).is_err());
        assert!(s.open("openai", b"sk-abc").is_err());
        assert!(!SecretSealer::is_sealed(b"sk-abc"));
    }

    #[test]
    fn round_trip_binds_key_and_name() {
        let s = SecretSealer::from_key(Zeroizing::new([7; 32]));
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        assert_eq!(s.key_memory(), KeyMemory::Heap);
        check(&s, &SecretSealer::from_key(Zeroizing::new([8; 32])));
    }

    #[test]
    fn heap_keys_seal_like_locked_keys() {
        let heap = SecretSealer::heap(Zeroizing::new([7; 32]));
        assert_eq!(heap.key_memory(), KeyMemory::Heap);
        assert_eq!(heap.key_memory().as_str(), "heap");
        check(&heap, &SecretSealer::heap(Zeroizing::new([8; 32])));
        // the memory does not change the format: a value sealed under one opens under
        // the other with the same key
        let locked = SecretSealer::from_key(Zeroizing::new([7; 32]));
        let sealed = heap.seal("gw", b"v").unwrap();
        assert_eq!(&*locked.open("gw", &sealed).unwrap(), b"v");
        assert_eq!(
            &*heap.open("gw", &locked.seal("gw", b"w").unwrap()).unwrap(),
            b"w"
        );
    }

    #[test]
    fn generated_keys_are_random() {
        let a = SecretSealer::generate_key().unwrap();
        let b = SecretSealer::generate_key().unwrap();
        assert_ne!(*a, *b);
    }

    #[cfg(feature = "encryption")]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn repository_key_inputs_stay_locked() {
        use crate::crypto::{LocalKey, LocalKeySource};
        let key = LocalKey::new("secrets", LocalKeySource::File, [7; 32]).unwrap();
        let s = SecretSealer::new(&key);
        assert_eq!(s.key_memory(), KeyMemory::Locked);
        let sealed = s.seal("gw", b"v").unwrap();
        let same = SecretSealer::heap(Zeroizing::new([7; 32]));
        assert_eq!(&*same.open("gw", &sealed).unwrap(), b"v");
    }
}
