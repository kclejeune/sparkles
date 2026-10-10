//! Graph IRIs and minted entity IRIs (spec C18 §8.10.2).
//!
//! A graph's IRI is the base, the principal, the harness, the project key and the
//! file's key. Every entity the import mints has a version 5 UUID IRI computed from the
//! dataset's id and a fixed string, so the same file always names the same entity, and
//! a link to a memory that does not exist yet already has the IRI the memory will get.

use sha1::{Digest as _, Sha1};
use sha2::Sha256;

/// What every IRI of one import depends on.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// `imports.base` of the dataset's memory settings, ending in `/` or `#`
    pub base: String,
    /// the name `/$/whoami` returns for the caller
    pub principal: String,
    /// the dataset's id
    pub dataset_id: uuid::Uuid,
}

/// A version 5 UUID (RFC 9562 §5.5) in the namespace `ns`, as `assert_facts` mints them.
pub fn uuid_v5(ns: &uuid::Uuid, name: &str) -> uuid::Uuid {
    let mut h = Sha1::new();
    h.update(ns.as_bytes());
    h.update(name.as_bytes());
    let d = h.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    uuid::Builder::from_sha1_bytes(b).into_uuid()
}

/// One IRI segment: unreserved characters and `.` kept, everything else
/// percent-encoded.
pub fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `sha256:<hex>` of some bytes.
pub fn digest(bytes: &[u8]) -> String {
    let h = Sha256::digest(bytes);
    let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

impl Ctx {
    /// The graphs of one principal and harness: `<base><principal>/<harness>/`.
    pub fn harness_prefix(&self, harness: &str) -> String {
        format!("{}{}/{harness}/", self.base, segment(&self.principal))
    }

    /// `<base><principal>/<harness>/<project>/`, the prefix of one project's graphs (or
    /// of the user scope's, with `user`).
    pub fn area(&self, harness: &str, project_segment: &str) -> String {
        format!(
            "{}{}/",
            self.harness_prefix(harness),
            segment(project_segment)
        )
    }

    /// `<area>memory/<key>`, `<area>index` or `<area>instructions/<key>`.
    pub fn graph(&self, harness: &str, project_segment: &str, tail: &str) -> String {
        format!("{}{tail}", self.area(harness, project_segment))
    }

    fn mint(&self, name: &str) -> String {
        format!("urn:uuid:{}", uuid_v5(&self.dataset_id, name).hyphenated())
    }

    /// A memory's IRI: from the principal, the harness, the project key and its key.
    pub fn memory_iri(&self, harness: &str, project_key: &str, key: &str) -> String {
        self.mint(&format!(
            "mem\0memory\0{}\0{harness}\0{project_key}\0{key}",
            self.principal
        ))
    }

    /// An instruction file's IRI.
    pub fn instructions_iri(&self, harness: &str, project_key: &str, key: &str) -> String {
        self.mint(&format!(
            "mem\0instructions\0{}\0{harness}\0{project_key}\0{key}",
            self.principal
        ))
    }

    /// The IRI of an `@path` import that is not followed: it names the path as written.
    pub fn dangling_iri(&self, harness: &str, project_key: &str, raw: &str) -> String {
        self.mint(&format!(
            "mem\0dangling\0{}\0{harness}\0{project_key}\0{raw}",
            self.principal
        ))
    }

    /// A project's IRI: from its key alone, so every harness and every person share it.
    pub fn project_iri(&self, project_key: &str) -> String {
        self.mint(&format!("mem\0project\0{project_key}"))
    }

    /// A session's IRI: from the principal, the harness and the session's id.
    pub fn session_iri(&self, harness: &str, id: &str) -> String {
        self.mint(&format!(
            "mem\0session\0{}\0{harness}\0{id}",
            self.principal
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphs_and_iris() {
        let c = Ctx {
            base: "https://example.org/memory/import/".into(),
            principal: "ana".into(),
            dataset_id: uuid::Uuid::nil(),
        };
        assert_eq!(
            c.graph("claude-code", "github.com.acme.shop", "memory/staging-db"),
            "https://example.org/memory/import/ana/claude-code/github.com.acme.shop/memory/staging-db"
        );
        assert_eq!(segment("a b/ü"), "a%20b%2F%C3%BC");
        let a = c.memory_iri("claude-code", "github.com/acme/shop", "x");
        assert_eq!(a, c.memory_iri("claude-code", "github.com/acme/shop", "x"));
        assert_ne!(a, c.memory_iri("claude-code", "github.com/acme/shop", "y"));
        let kai = Ctx {
            principal: "kai".into(),
            ..c.clone()
        };
        assert_eq!(c.project_iri("p"), kai.project_iri("p"));
        assert_ne!(
            a,
            kai.memory_iri("claude-code", "github.com/acme/shop", "x")
        );
        assert!(a.starts_with("urn:uuid:"));
        assert_eq!(digest(b"").len(), 7 + 64);
    }
}
