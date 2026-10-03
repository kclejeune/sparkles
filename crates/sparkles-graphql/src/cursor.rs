//! Connection cursors (§5.2): base64url JSON naming the commit a page was read at, a
//! hash of the field and its arguments, and the position after the item.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as J};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub v: u32,
    /// the commit the page was read at
    pub c: u64,
    /// the hash of the field's path, its arguments other than cursors and slices, and
    /// the schema version
    pub h: String,
    /// the position after the item
    pub o: u64,
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn unb64url(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xff) as u8);
        }
    }
    Some(out)
}

pub fn encode(c: &Cursor) -> String {
    b64url(&serde_json::to_vec(c).unwrap_or_default())
}

pub fn decode(s: &str) -> Result<Cursor, String> {
    let bytes = unb64url(s).ok_or("not a cursor of this endpoint")?;
    let c: Cursor =
        serde_json::from_slice(&bytes).map_err(|_| "not a cursor of this endpoint".to_string())?;
    if c.v != 1 {
        return Err(format!("cursor version {} is not supported", c.v));
    }
    Ok(c)
}

/// The `h` of a connection field: its name, its arguments other than `first`, `last`,
/// `after`, `before` and `offset`, and the schema version.
pub fn field_hash(field: &str, args: &Map<String, J>, version: u64) -> String {
    let mut kept: Vec<(&String, &J)> = args
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "first" | "last" | "after" | "before" | "offset"))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(b.0));
    let mut h = Sha256::new();
    h.update(field.as_bytes());
    h.update([0]);
    h.update(serde_json::to_vec(&kept).unwrap_or_default());
    h.update([0]);
    h.update(version.to_le_bytes());
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for o in [0u64, 1, 200, 99999] {
            let c = Cursor {
                v: 1,
                c: 1287,
                h: "4f2a".into(),
                o,
            };
            assert_eq!(decode(&encode(&c)).unwrap(), c);
        }
        assert!(decode("!!").is_err());
        assert!(decode("e30").is_err());
    }
}
