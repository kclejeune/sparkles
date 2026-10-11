//! Sealed values of a server's runtime secrets, one small value per file.
//!
//! A sealed value is one line of text: [`SEALED_SECRET_PREFIX`], the base64 of a fresh
//! 16-byte salt and the base64 of the AES-256-GCM ciphertext with its tag. The value
//! key is derived from the operator's key and the salt with HKDF-SHA256, so the zero
//! nonce is used once under each derived key, as for the repository objects. The
//! secret's name is part of the authenticated data, so a sealed file renamed to another
//! secret's name does not open.

use super::memory::LockedKey;
use super::objects::{decode_bounded, invalid};
use super::{LocalKey, primitive};
use crate::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::sync::Arc;
use zeroize::Zeroizing;

/// The first bytes of a sealed secret, including its format version.
pub const SEALED_SECRET_PREFIX: &str = "sparkles-sealed-secret/1 ";

const INFO: &[u8] = b"sparkles/secrets/value";
const MAX_PLAIN: usize = 64 * 1024;

fn aad(name: &str) -> Vec<u8> {
    let mut out = b"sparkles/secrets/1\0".to_vec();
    out.extend_from_slice(name.as_bytes());
    out
}

/// Seals and opens runtime secret values with one operator key.
#[derive(Clone)]
pub struct SecretSealer {
    key: Arc<LockedKey>,
}

impl std::fmt::Debug for SecretSealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretSealer([redacted])")
    }
}

impl SecretSealer {
    /// A sealer that uses the key of `key`, which stays in its protected memory.
    pub fn new(key: &LocalKey) -> Self {
        Self {
            key: key.secret.clone(),
        }
    }

    /// Whether `stored` is a sealed value rather than a plaintext one.
    pub fn is_sealed(stored: &[u8]) -> bool {
        stored.starts_with(SEALED_SECRET_PREFIX.as_bytes())
    }

    /// The sealed form of the value `plain` of secret `name`.
    pub fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        if plain.len() > MAX_PLAIN {
            return Err(invalid("secret value is too long"));
        }
        let salt = primitive::random::<16>()?;
        let key = primitive::derive(self.key.as_ref().as_ref(), &salt, INFO)?;
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
        let key = primitive::derive(self.key.as_ref().as_ref(), &salt, INFO)?;
        primitive::open(key.as_ref(), [0; 12], &aad(name), &ct)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::*;
    use crate::crypto::LocalKeySource;

    fn sealer(byte: u8) -> SecretSealer {
        SecretSealer::new(&LocalKey::new("secrets", LocalKeySource::File, [byte; 32]).unwrap())
    }

    #[test]
    fn round_trip_binds_key_and_name() {
        let s = sealer(7);
        let sealed = s.seal("openai", b"sk-abc").unwrap();
        assert!(SecretSealer::is_sealed(&sealed));
        assert!(!sealed.windows(6).any(|w| w == b"sk-abc"));
        assert_eq!(&*s.open("openai", &sealed).unwrap(), b"sk-abc");
        // a fresh salt each time
        assert_ne!(sealed, s.seal("openai", b"sk-abc").unwrap());
        assert!(s.open("anthropic", &sealed).is_err());
        assert!(sealer(8).open("openai", &sealed).is_err());
        let mut changed = sealed.clone();
        let at = changed.len() - 4;
        changed[at] = if changed[at] == b'A' { b'B' } else { b'A' };
        assert!(s.open("openai", &changed).is_err());
        assert!(s.open("openai", b"sk-abc").is_err());
        assert!(!SecretSealer::is_sealed(b"sk-abc"));
    }
}
