//! HMAC-SHA256 (RFC 2104) and the signature of Standard Webhooks (spec C21 §4.1).

use base64::Engine as _;
use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

/// HMAC-SHA256 of `msg` under `key`, as RFC 2104 defines it.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// The key of a signing secret: the Base64 after `whsec_`, as Standard Webhooks writes
/// keys, or else the secret's bytes.
pub fn key_of(secret: &str) -> Vec<u8> {
    secret
        .strip_prefix("whsec_")
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .unwrap_or_else(|| secret.as_bytes().to_vec())
}

/// The `webhook-signature` header: `v1,` and the Base64 HMAC-SHA256 of
/// `{id}.{timestamp}.{body}`.
pub fn standard(key: &[u8], id: &str, timestamp: i64, body: &[u8]) -> String {
    let mut msg = format!("{id}.{timestamp}.").into_bytes();
    msg.extend_from_slice(body);
    let mac = hmac_sha256(key, &msg);
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// RFC 4231's test cases 1, 2 and 6 (a key longer than the block).
    #[test]
    fn rfc4231_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// The example of the Standard Webhooks reference libraries.
    #[test]
    fn standard_webhooks_example() {
        let key = key_of("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw");
        let sig = standard(
            &key,
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            1614265330,
            br#"{"test": 2432232314}"#,
        );
        assert_eq!(sig, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
        assert_eq!(key_of("plain"), b"plain");
    }
}
