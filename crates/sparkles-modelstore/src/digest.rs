//! File digests: SHA-256, and the Git blob SHA-1 the Hugging Face Hub reports for files
//! that are not stored in Git LFS.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest as _, Sha256};

/// The expected digest of a file.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Digest {
    /// SHA-256 of the file's bytes (Git LFS object ids, declared hashes, Nix).
    Sha256([u8; 32]),
    /// SHA-1 of `"blob <len>\0"` followed by the file's bytes, which is Git's object id
    /// of the file.
    GitBlobSha1([u8; 20]),
}

impl Digest {
    /// Parse a hex SHA-256, optionally prefixed with `sha256:`.
    pub fn sha256_hex(s: &str) -> Option<Digest> {
        let s = s.strip_prefix("sha256:").unwrap_or(s);
        let mut out = [0u8; 32];
        unhex(s, &mut out).then_some(Digest::Sha256(out))
    }

    /// Parse a hex Git blob id.
    pub fn git_blob_hex(s: &str) -> Option<Digest> {
        let mut out = [0u8; 20];
        unhex(s, &mut out).then_some(Digest::GitBlobSha1(out))
    }

    /// `sha256:<hex>` or `git-sha1:<hex>`.
    pub fn to_tagged(&self) -> String {
        match self {
            Digest::Sha256(b) => format!("sha256:{}", hex(b)),
            Digest::GitBlobSha1(b) => format!("git-sha1:{}", hex(b)),
        }
    }

    /// The inverse of [`Digest::to_tagged`].
    pub fn from_tagged(s: &str) -> Option<Digest> {
        if let Some(h) = s.strip_prefix("sha256:") {
            Digest::sha256_hex(h)
        } else {
            s.strip_prefix("git-sha1:").and_then(Digest::git_blob_hex)
        }
    }

    /// Compute the same kind of digest over a file.
    pub fn of_file(&self, path: &Path) -> io::Result<Digest> {
        let mut f = File::open(path)?;
        match self {
            Digest::Sha256(_) => sha256_reader(&mut f).map(Digest::Sha256),
            Digest::GitBlobSha1(_) => {
                let len = f.metadata()?.len();
                git_blob_reader(&mut f, len).map(Digest::GitBlobSha1)
            }
        }
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_tagged())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_tagged())
    }
}

/// SHA-256 of everything `r` yields.
pub fn sha256_reader(r: &mut dyn Read) -> io::Result<[u8; 32]> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// The Git blob id of `len` bytes read from `r`.
pub fn git_blob_reader(r: &mut dyn Read, len: u64) -> io::Result<[u8; 20]> {
    let mut h = Sha1::new();
    h.update(format!("blob {len}\0").as_bytes());
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

pub(crate) fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

fn unhex(s: &str, out: &mut [u8]) -> bool {
    let b = s.as_bytes();
    if b.len() != out.len() * 2 {
        return false;
    }
    let v = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    for (i, o) in out.iter_mut().enumerate() {
        match (v(b[2 * i]), v(b[2 * i + 1])) {
            (Some(h), Some(l)) => *o = (h << 4) | l,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // sha256("abc")
        let d = sha256_reader(&mut &b"abc"[..]).unwrap();
        assert_eq!(
            hex(&d),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // `git hash-object` of a file holding "hello\n"
        let g = git_blob_reader(&mut &b"hello\n"[..], 6).unwrap();
        assert_eq!(hex(&g), "ce013625030ba8dba906f756967f9e9ca394464a");
        let t = Digest::GitBlobSha1(g).to_tagged();
        assert_eq!(Digest::from_tagged(&t), Some(Digest::GitBlobSha1(g)));
        assert!(Digest::sha256_hex("zz").is_none());
    }
}
