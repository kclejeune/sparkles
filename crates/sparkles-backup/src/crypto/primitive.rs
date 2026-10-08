//! Scheme 1 primitives. Every encrypted object gets fresh salt and its own derived
//! AES key; the zero nonce is used exactly once under that derived key.

use crate::{BackupError, Code, Result};
use aws_lc_rs::{aead, hkdf, hmac};
use zeroize::Zeroizing;

pub const KEY_BYTES: usize = 32;
pub const TAG_BYTES: usize = 16;

fn crypto_error() -> BackupError {
    BackupError::new(Code::Internal, "repository cryptography failed")
}

struct KeyLength;
impl hkdf::KeyType for KeyLength {
    fn len(&self) -> usize {
        KEY_BYTES
    }
}

/// Random public values such as salts and nonces. Use [`random_key`] for secrets,
/// because this returns an ordinary array that is copied without zeroization.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(bytes.as_mut()).map_err(|_| crypto_error())?;
    Ok(bytes)
}

/// A random secret key, filled in place inside its zeroizing buffer.
pub fn random_key() -> Result<Zeroizing<[u8; KEY_BYTES]>> {
    let mut bytes = Zeroizing::new([0; KEY_BYTES]);
    getrandom::fill(bytes.as_mut()).map_err(|_| crypto_error())?;
    Ok(bytes)
}

pub fn derive(secret: &[u8], salt: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; KEY_BYTES]>> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(secret);
    let context = [info];
    let okm = prk
        .expand(&context, KeyLength)
        .map_err(|_| crypto_error())?;
    let mut out = Zeroizing::new([0; KEY_BYTES]);
    okm.fill(out.as_mut()).map_err(|_| crypto_error())?;
    Ok(out)
}

pub fn mac(key: &[u8], bytes: &[u8]) -> [u8; KEY_BYTES] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), bytes);
    let mut out = [0; KEY_BYTES];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Constant-time HMAC-SHA256 tag check.
pub fn mac_verify(key: &[u8], bytes: &[u8], tag: &[u8]) -> bool {
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, key), bytes, tag).is_ok()
}

pub fn seal(key: &[u8], nonce: [u8; 12], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| crypto_error())?,
    );
    let mut out = Zeroizing::new(plain.to_vec());
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad),
        &mut *out,
    )
    .map_err(|_| crypto_error())?;
    Ok(std::mem::take(&mut *out))
}

pub fn open(key: &[u8], nonce: [u8; 12], aad: &[u8], ct: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| crypto_error())?,
    );
    let mut out = Zeroizing::new(ct.to_vec());
    let len = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut out,
        )
        .map_err(|_| BackupError::invalid_backup("encryption", "authentication failed"))?
        .len();
    out.truncate(len);
    Ok(out)
}

/// Integer PADME, including empty inputs and checked overflow.
pub fn padme(len: u64) -> Result<u64> {
    if len <= 1 {
        return Ok(len);
    }
    let exponent = 63 - len.leading_zeros();
    let significant = 32 - exponent.leading_zeros();
    let bits = exponent.saturating_sub(significant);
    let mask = (1u64 << bits) - 1;
    len.checked_add(mask)
        .map(|n| n & !mask)
        .ok_or_else(|| BackupError::invalid_backup("padding", "length overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes256gcm_nist_empty_plaintext_fixture_and_authenticated_aad() {
        // NIST SP800-38D AES256GCM zero key/nonce, empty input.
        let sealed = seal(&[0; 32], [0; 12], &[], &[]).unwrap();
        assert_eq!(
            crate::blob::hex(&sealed),
            "530f8afbc74536b9a963b4f1c4cb738b"
        );
        assert!(open(&[0; 32], [0; 12], &[], &sealed).unwrap().is_empty());
        assert!(open(&[0; 32], [0; 12], b"changed", &sealed).is_err());
    }

    #[test]
    fn hkdf_rfc5869_and_hmac_rfc4231_fixtures() {
        let secret = [0x0b; 22];
        let salt: Vec<_> = (0..13).collect();
        let info: Vec<_> = (0xf0..=0xf9).collect();
        let key = derive(&secret, &salt, &info).unwrap();
        assert_eq!(
            crate::blob::hex(key.as_ref()),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        );
        assert_eq!(
            crate::blob::hex(&mac(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn padme_rounding_is_monotone_and_bounded() {
        for len in 0..65536 {
            let padded = padme(len).unwrap();
            assert!(padded >= len);
            assert!(padded <= len.saturating_add(len / 8 + 1));
            assert!(padme(padded).unwrap() == padded);
        }
        assert_eq!(padme(0).unwrap(), 0);
        assert_eq!(padme(1).unwrap(), 1);
        assert!(padme(u64::MAX).is_err());
    }
}
