//! Minted API tokens: `<data>/auth/tokens.json` (hashes only), lookup by digest, and
//! `lastUsed` kept in memory until the next write.

use super::store::{self, parse_rfc3339, rfc3339};
use super::{Identity, Scope, crypto};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Links of a token chain (a token minted by a token minted by …).
pub const MAX_CHAIN: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hostname: String,
}

/// One minted token. The token itself is never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    /// `sha256:<hex>` of the whole token string
    pub hash: String,
    pub owner: Identity,
    /// the token that minted this one
    #[serde(default)]
    pub parent: Option<String>,
    pub scope: Scope,
    pub created: String,
    pub expires: String,
    /// `api`, `ui`, `cli-loopback` or `cli-device`
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

impl TokenRecord {
    pub fn expires_at(&self) -> i64 {
        parse_rfc3339(&self.expires).unwrap_or(0)
    }
}

#[derive(Serialize, Deserialize)]
struct TokenFile {
    format: u32,
    tokens: Vec<TokenRecord>,
}

#[derive(Clone, Default)]
struct Inner {
    by_id: BTreeMap<String, TokenRecord>,
    by_digest: HashMap<[u8; 32], String>,
    /// Unix seconds of the last use, not yet written
    last_used: HashMap<String, i64>,
}

pub struct TokenStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

/// `tok_` and 12 lowercase base32 characters (60 random bits).
pub fn new_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let r = crypto::random_bytes(8);
    let mut v = u64::from_le_bytes(r.try_into().unwrap_or([0; 8]));
    let mut s = String::from("tok_");
    for _ in 0..12 {
        s.push(ALPHABET[(v & 31) as usize] as char);
        v >>= 5;
    }
    s
}

impl TokenStore {
    /// Open (or start) the store; a corrupt file is an error, never silently reset.
    pub fn open(path: PathBuf) -> Result<TokenStore> {
        let mut inner = Inner::default();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let f: TokenFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is corrupt", path.display()))?;
                if f.format != 1 {
                    bail!("{}: unknown format {}", path.display(), f.format);
                }
                for t in f.tokens {
                    let d = super::config::parse_token_hash(&t.hash)
                        .with_context(|| format!("{}: bad hash of {}", path.display(), t.id))?;
                    inner.by_digest.insert(d, t.id.clone());
                    inner.by_id.insert(t.id.clone(), t);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
        Ok(TokenStore {
            path,
            inner: Mutex::new(inner),
        })
    }

    /// Write `next` and only then make it the in-memory state, so that a failed write
    /// leaves memory matching the file. Otherwise a revoked token would vanish from memory
    /// and authenticate again after a restart.
    fn commit(&self, inner: &mut Inner, mut next: Inner) -> Result<()> {
        self.save(&mut next)?;
        *inner = next;
        Ok(())
    }

    fn save(&self, inner: &mut Inner) -> Result<()> {
        for (id, t) in std::mem::take(&mut inner.last_used) {
            if let Some(r) = inner.by_id.get_mut(&id) {
                r.last_used = Some(rfc3339(t));
            }
        }
        let f = TokenFile {
            format: 1,
            tokens: inner.by_id.values().cloned().collect(),
        };
        store::write_private(&self.path, &serde_json::to_vec_pretty(&f)?)
    }

    /// Write pending `lastUsed` times (graceful shutdown).
    pub fn flush(&self) -> Result<()> {
        let mut inner = self.inner.lock();
        if inner.last_used.is_empty() {
            return Ok(());
        }
        let next = inner.clone();
        self.commit(&mut inner, next)
    }

    pub fn by_digest(&self, d: &[u8; 32]) -> Option<TokenRecord> {
        let inner = self.inner.lock();
        let id = inner.by_digest.get(d)?;
        inner.by_id.get(id).cloned()
    }

    pub fn get(&self, id: &str) -> Option<TokenRecord> {
        self.inner.lock().by_id.get(id).cloned()
    }

    pub fn touch(&self, id: &str, now: i64) {
        self.inner.lock().last_used.insert(id.to_string(), now);
    }

    pub fn last_used(&self, id: &str) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .last_used
            .get(id)
            .map(|t| rfc3339(*t))
            .or_else(|| inner.by_id.get(id).and_then(|r| r.last_used.clone()))
    }

    /// Store a new record (expired records are pruned on the way).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert(&self, rec: TokenRecord, now: i64) -> Result<()> {
        self.insert_within(rec, now, usize::MAX).map(|_| ())
    }

    /// Store a new record unless its owner already has `max` unexpired tokens; `false`:
    /// not stored.
    pub fn insert_within(&self, rec: TokenRecord, now: i64, max: usize) -> Result<bool> {
        let digest = super::config::parse_token_hash(&rec.hash).context("token hash")?;
        let mut inner = self.inner.lock();
        let mut next = inner.clone();
        prune(&mut next, now);
        let owned = next
            .by_id
            .values()
            .filter(|t| t.owner.kind == rec.owner.kind && t.owner.name == rec.owner.name)
            .count();
        if owned >= max {
            return Ok(false);
        }
        next.by_digest.insert(digest, rec.id.clone());
        next.by_id.insert(rec.id.clone(), rec);
        self.commit(&mut inner, next).map(|_| true)
    }

    /// Remove tokens, and the tokens they minted. Returns how many were removed.
    pub fn remove(&self, ids: &[String]) -> Result<usize> {
        let mut inner = self.inner.lock();
        let mut gone: Vec<String> = ids.to_vec();
        let mut i = 0;
        while i < gone.len() {
            let children: Vec<String> = inner
                .by_id
                .values()
                .filter(|t| t.parent.as_deref() == Some(gone[i].as_str()))
                .map(|t| t.id.clone())
                .collect();
            for c in children {
                if !gone.contains(&c) {
                    gone.push(c);
                }
            }
            i += 1;
        }
        let mut next = inner.clone();
        let mut n = 0;
        for id in &gone {
            if let Some(r) = next.by_id.remove(id) {
                n += 1;
                if let Some(d) = super::config::parse_token_hash(&r.hash) {
                    next.by_digest.remove(&d);
                }
                next.last_used.remove(id);
            }
        }
        if n > 0 {
            self.commit(&mut inner, next)?;
        }
        Ok(n)
    }

    /// Give an unexpired token a new secret (its record, scope and expiry stay); the old
    /// secret stops working. `None` when the token is gone or expired.
    pub fn reissue(&self, id: &str, now: i64) -> Result<Option<zeroize::Zeroizing<String>>> {
        let token = zeroize::Zeroizing::new(super::policy::new_token());
        let hash = super::policy::token_hash(&token);
        let digest = super::config::parse_token_hash(&hash).context("token hash")?;
        let mut inner = self.inner.lock();
        let mut next = inner.clone();
        let Some(rec) = next.by_id.get_mut(id).filter(|r| r.expires_at() > now) else {
            return Ok(None);
        };
        let old = std::mem::replace(&mut rec.hash, hash);
        if let Some(d) = super::config::parse_token_hash(&old) {
            next.by_digest.remove(&d);
        }
        next.by_digest.insert(digest, id.to_string());
        self.commit(&mut inner, next)?;
        Ok(Some(token))
    }

    /// Record the groups an OIDC or proxy identity has now in the tokens it owns, so that
    /// their permissions follow the provider. Returns how many tokens changed.
    pub fn refresh_groups(&self, who: &Identity) -> Result<usize> {
        let mut inner = self.inner.lock();
        let mut next = inner.clone();
        let mut n = 0;
        for t in next.by_id.values_mut() {
            if t.owner.kind == who.kind && t.owner.name == who.name && t.owner.groups != who.groups
            {
                t.owner.groups = who.groups.clone();
                n += 1;
            }
        }
        if n > 0 {
            self.commit(&mut inner, next)?;
        }
        Ok(n)
    }

    /// Records matching `f`, ordered by id.
    pub fn list(&self, f: impl Fn(&TokenRecord) -> bool) -> Vec<TokenRecord> {
        self.inner
            .lock()
            .by_id
            .values()
            .filter(|t| f(t))
            .cloned()
            .collect()
    }

    /// Unexpired tokens.
    pub fn active(&self, now: i64) -> usize {
        self.inner
            .lock()
            .by_id
            .values()
            .filter(|t| t.expires_at() > now)
            .count()
    }
}

fn prune(inner: &mut Inner, now: i64) {
    let expired: Vec<String> = inner
        .by_id
        .values()
        .filter(|t| t.expires_at() <= now)
        .map(|t| t.id.clone())
        .collect();
    for id in expired {
        if let Some(r) = inner.by_id.remove(&id)
            && let Some(d) = super::config::parse_token_hash(&r.hash)
        {
            inner.by_digest.remove(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, parent: Option<&str>, token: &str) -> TokenRecord {
        TokenRecord {
            id: id.into(),
            name: "t".into(),
            hash: super::super::policy::token_hash(token),
            owner: Identity {
                kind: super::super::Kind::User,
                name: "bob".into(),
                groups: vec![],
                display_name: None,
            },
            parent: parent.map(str::to_string),
            scope: Scope::all(),
            created: rfc3339(0),
            expires: rfc3339(4_000_000_000),
            via: "api".into(),
            client: None,
            last_used: None,
        }
    }

    #[test]
    fn store_roundtrip_and_cascading_revocation() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("tokens.json");
        let s = TokenStore::open(path.clone()).unwrap();
        let (a, b) = (
            super::super::policy::new_token(),
            super::super::policy::new_token(),
        );
        s.insert(rec("tok_a", None, &a), 0).unwrap();
        s.insert(rec("tok_b", Some("tok_a"), &b), 0).unwrap();
        s.touch("tok_a", 1_800_000_000);
        s.flush().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("spk_"), "{text}");
        let s2 = TokenStore::open(path.clone()).unwrap();
        let da = crypto::sha256(a.as_bytes());
        assert_eq!(s2.by_digest(&da).unwrap().id, "tok_a");
        assert_eq!(
            s2.last_used("tok_a").as_deref(),
            Some(rfc3339(1_800_000_000).as_str())
        );
        assert_eq!(s2.remove(&["tok_a".into()]).unwrap(), 2);
        assert!(s2.by_digest(&da).is_none());
        assert!(s2.get("tok_b").is_none());
        std::fs::write(&path, "{not json").unwrap();
        assert!(TokenStore::open(path).is_err());
        let id = new_id();
        assert!(
            regex::Regex::new("^tok_[a-z2-7]{12}$")
                .unwrap()
                .is_match(&id),
            "{id}"
        );
    }

    /// A save that fails must leave memory as it was: a revoked token still works, a
    /// reissued token keeps its old secret and a refused mint is not visible.
    #[test]
    fn failed_save_leaves_memory_unchanged() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("tokens.json");
        let s = TokenStore::open(path.clone()).unwrap();
        let a = super::super::policy::new_token();
        s.insert(rec("tok_a", None, &a), 0).unwrap();
        let da = crypto::sha256(a.as_bytes());
        // A directory where the writer wants its temporary file makes every save fail.
        let tmp = d.path().join("tokens.json.tmp");
        std::fs::create_dir(&tmp).unwrap();

        assert!(s.remove(&["tok_a".into()]).is_err());
        assert_eq!(s.by_digest(&da).unwrap().id, "tok_a");
        assert!(s.reissue("tok_a", 0).is_err());
        assert_eq!(s.by_digest(&da).unwrap().id, "tok_a");
        let c = super::super::policy::new_token();
        assert!(s.insert(rec("tok_c", None, &c), 0).is_err());
        assert!(s.get("tok_c").is_none());
        assert!(s.by_digest(&crypto::sha256(c.as_bytes())).is_none());
        let who = Identity {
            groups: vec!["g".into()],
            ..s.get("tok_a").unwrap().owner
        };
        assert!(s.refresh_groups(&who).is_err());
        assert!(s.get("tok_a").unwrap().owner.groups.is_empty());
        s.touch("tok_a", 1_800_000_000);
        assert!(s.flush().is_err());
        assert_eq!(
            s.last_used("tok_a").as_deref(),
            Some(rfc3339(1_800_000_000).as_str())
        );

        // Once the disk works again, memory and file agree.
        std::fs::remove_dir(&tmp).unwrap();
        assert_eq!(s.remove(&["tok_a".into()]).unwrap(), 1);
        assert!(s.by_digest(&da).is_none());
        let s2 = TokenStore::open(path).unwrap();
        assert!(s2.by_digest(&da).is_none());
    }
}
