//! UI sessions: `<data>/auth/sessions.json` (hashed ids), the session key, signed
//! cookies and CSRF tokens.

use super::store::{self, parse_rfc3339, rfc3339};
use super::{Identity, crypto};
use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, HeaderValue, header};
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Sessions kept at most. When the store is full, the owner that holds the most loses its
/// oldest, never anyone else.
pub const MAX_SESSIONS: usize = 10_000;
/// Sessions one owner keeps at most (a user, an OIDC or proxy identity; a token login
/// counts for the token's owner): a new one ends the owner's oldest, so that no one can
/// push the others' sessions out.
pub const MAX_SESSIONS_PER_OWNER: usize = 50;

/// Subkeys derived from the session key file.
pub struct Keys {
    cookie: [u8; 32],
    csrf: [u8; 32],
}

impl Keys {
    /// Read the key file, creating it (64 random bytes, base64, mode 0600) when absent.
    pub fn load_or_create(path: &Path) -> Result<Keys> {
        let key = match std::fs::read_to_string(path) {
            Ok(text) => base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .with_context(|| format!("{}: not base64", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let k = crypto::random_bytes(64);
                if let Some(dir) = path.parent() {
                    store::private_dir(dir)?;
                }
                store::write_private(
                    path,
                    base64::engine::general_purpose::STANDARD
                        .encode(&k)
                        .as_bytes(),
                )?;
                k
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if key.len() < 32 {
            bail!(
                "{}: the session key must be at least 32 bytes",
                path.display()
            );
        }
        Ok(Keys {
            cookie: crypto::hmac_sha256(&key, b"cookie"),
            csrf: crypto::hmac_sha256(&key, b"csrf"),
        })
    }

    /// `value.signature`, both base64url.
    pub fn sign(&self, name: &str, value: &str) -> String {
        let sig = crypto::hmac_sha256(&self.cookie, format!("{name}={value}").as_bytes());
        format!("{value}.{}", crypto::b64url(&sig))
    }

    /// The value of a signed cookie, if the signature is valid.
    pub fn verify<'a>(&self, name: &str, signed: &'a str) -> Option<&'a str> {
        let (value, sig) = signed.rsplit_once('.')?;
        let want = crypto::hmac_sha256(&self.cookie, format!("{name}={value}").as_bytes());
        crypto::ct_eq(crypto::b64url(&want).as_bytes(), sig.as_bytes()).then_some(value)
    }

    /// The CSRF token bound to a session id or a proxy principal.
    pub fn csrf(&self, binding: &str) -> String {
        crypto::b64url(&crypto::hmac_sha256(&self.csrf, binding.as_bytes()))
    }
}

/// How a session was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Oidc,
    Password,
    Token,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Oidc => "oidc",
            Method::Password => "password",
            Method::Token => "token",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    /// `sha256:<hex>` of the session id
    pub id: String,
    pub method: Method,
    pub principal: Identity,
    /// a token login: the session dies with the token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    pub created: String,
    pub expires: String,
    /// creation order (memory only; `created` has whole seconds)
    #[serde(skip)]
    seq: u64,
}

impl SessionRecord {
    pub fn expires_at(&self) -> i64 {
        parse_rfc3339(&self.expires).unwrap_or(0)
    }

    fn owner(&self) -> String {
        self.principal.log_name()
    }
}

#[derive(Serialize, Deserialize)]
struct SessionFile {
    format: u32,
    sessions: Vec<SessionRecord>,
}

pub struct SessionStore {
    path: PathBuf,
    inner: Mutex<HashMap<[u8; 32], SessionRecord>>,
    /// ID tokens of OIDC sessions (memory only), for RP-initiated logout
    id_tokens: Mutex<HashMap<[u8; 32], String>>,
    next_seq: AtomicU64,
    /// [`MAX_SESSIONS`] and [`MAX_SESSIONS_PER_OWNER`]
    caps: (usize, usize),
}

/// Drop the oldest session of the owner that holds the most (of those tied, the one
/// with the oldest session); returns its digest.
fn evict_one(map: &mut HashMap<[u8; 32], SessionRecord>) -> Option<[u8; 32]> {
    // owner → (sessions, oldest seq, its digest)
    let mut owners: HashMap<String, (usize, u64, [u8; 32])> = HashMap::new();
    for (k, s) in map.iter() {
        let e = owners.entry(s.owner()).or_insert((0, u64::MAX, *k));
        e.0 += 1;
        if s.seq < e.1 {
            (e.1, e.2) = (s.seq, *k);
        }
    }
    let (_, _, k) = owners
        .into_values()
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))?;
    map.remove(&k);
    Some(k)
}

fn digest_of(id: &str) -> Option<[u8; 32]> {
    super::config::parse_token_hash(id)
}

impl SessionStore {
    pub fn open(path: PathBuf, now: i64) -> Result<SessionStore> {
        let mut map = HashMap::new();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let f: SessionFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is corrupt", path.display()))?;
                if f.format != 1 {
                    bail!("{}: unknown format {}", path.display(), f.format);
                }
                // the file lists them by creation
                for (seq, mut s) in f.sessions.into_iter().enumerate() {
                    let d = digest_of(&s.id)
                        .with_context(|| format!("{}: bad session id", path.display()))?;
                    s.seq = seq as u64;
                    if s.expires_at() > now {
                        map.insert(d, s);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
        let next = map.values().map(|s| s.seq + 1).max().unwrap_or(0);
        Ok(SessionStore {
            path,
            inner: Mutex::new(map),
            id_tokens: Mutex::new(HashMap::new()),
            next_seq: AtomicU64::new(next),
            caps: (MAX_SESSIONS, MAX_SESSIONS_PER_OWNER),
        })
    }

    /// Other caps than [`MAX_SESSIONS`] and [`MAX_SESSIONS_PER_OWNER`] (tests).
    #[cfg(test)]
    pub fn with_caps(mut self, total: usize, per_owner: usize) -> SessionStore {
        self.caps = (total, per_owner);
        self
    }

    fn save(&self, map: &HashMap<[u8; 32], SessionRecord>) -> Result<()> {
        let mut sessions: Vec<SessionRecord> = map.values().cloned().collect();
        sessions.sort_by_key(|s| s.seq);
        let f = SessionFile {
            format: 1,
            sessions,
        };
        store::write_private(&self.path, &serde_json::to_vec_pretty(&f)?)
    }

    /// Start a session; returns the raw session id (the cookie value before signing).
    pub fn create(
        &self,
        method: Method,
        principal: Identity,
        token_id: Option<String>,
        now: i64,
        expires: i64,
    ) -> Result<(String, [u8; 32])> {
        let raw = crypto::random_token(32);
        let digest = crypto::sha256(raw.as_bytes());
        let rec = SessionRecord {
            id: format!("sha256:{}", crypto::hex(&digest)),
            method,
            principal,
            token_id,
            created: rfc3339(now),
            expires: rfc3339(expires),
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
        };
        let (max, per_owner) = self.caps;
        let mut ended = Vec::new();
        let mut map = self.inner.lock();
        map.retain(|_, s| s.expires_at() > now);
        // the owner's oldest sessions beyond its cap
        let owner = rec.owner();
        let mut mine: Vec<(u64, [u8; 32])> = map
            .iter()
            .filter(|(_, s)| s.owner() == owner)
            .map(|(k, s)| (s.seq, *k))
            .collect();
        mine.sort_unstable();
        let excess = (mine.len() + 1).saturating_sub(per_owner.max(1));
        for (_, k) in mine.into_iter().take(excess) {
            map.remove(&k);
            ended.push(k);
        }
        // a full store: the biggest owner's oldest
        while map.len() >= max.max(1) {
            match evict_one(&mut map) {
                Some(k) => ended.push(k),
                None => break,
            }
        }
        map.insert(digest, rec);
        let saved = self.save(&map);
        drop(map);
        let mut ids = self.id_tokens.lock();
        for k in ended {
            ids.remove(&k);
        }
        drop(ids);
        saved?;
        Ok((raw, digest))
    }

    pub fn get(&self, digest: &[u8; 32], now: i64) -> Option<SessionRecord> {
        self.inner
            .lock()
            .get(digest)
            .filter(|s| s.expires_at() > now)
            .cloned()
    }

    pub fn remove(&self, digest: &[u8; 32]) -> Result<Option<SessionRecord>> {
        self.id_tokens.lock().remove(digest);
        let mut map = self.inner.lock();
        let r = map.remove(digest);
        if r.is_some() {
            self.save(&map)?;
        }
        Ok(r)
    }

    pub fn set_id_token(&self, digest: [u8; 32], t: String) {
        self.id_tokens.lock().insert(digest, t);
    }

    pub fn id_token(&self, digest: &[u8; 32]) -> Option<String> {
        self.id_tokens.lock().get(digest).cloned()
    }

    /// Drop expired sessions (hourly).
    pub fn prune(&self, now: i64) -> Result<()> {
        let mut map = self.inner.lock();
        let before = map.len();
        map.retain(|_, s| s.expires_at() > now);
        if map.len() != before {
            self.save(&map)?;
        }
        Ok(())
    }

    pub fn active(&self, now: i64) -> usize {
        self.inner
            .lock()
            .values()
            .filter(|s| s.expires_at() > now)
            .count()
    }
}

/// The value of cookie `name` in the request's `Cookie` headers.
pub fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim_matches('"'))
}

/// Cookie attributes of this deployment: `__Host-` names and `Secure` over https.
#[derive(Clone, Copy, Debug)]
pub struct CookieMode {
    pub secure: bool,
}

impl CookieMode {
    pub fn name(self, base: &str) -> String {
        if self.secure {
            format!("__Host-{base}")
        } else {
            base.to_string()
        }
    }

    /// A `Set-Cookie` value (`Max-Age=0` clears it).
    pub fn set(self, base: &str, value: &str, max_age: i64) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{}={value}; HttpOnly{secure}; SameSite=Lax; Path=/; Max-Age={max_age}",
            self.name(base)
        ))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_cookies_and_store() {
        let d = tempfile::tempdir().unwrap();
        let kf = d.path().join("auth/session.key");
        let k = Keys::load_or_create(&kf).unwrap();
        let signed = k.sign("s", "abc");
        assert_eq!(k.verify("s", &signed), Some("abc"));
        assert_eq!(k.verify("t", &signed), None);
        assert_eq!(k.verify("s", &signed.replace("abc", "abd")), None);
        let k2 = Keys::load_or_create(&kf).unwrap();
        assert_eq!(k2.verify("s", &signed), Some("abc"));
        assert_eq!(k.csrf("x"), k2.csrf("x"));
        assert_ne!(k.csrf("x"), k.csrf("y"));

        let path = d.path().join("auth/sessions.json");
        let s = SessionStore::open(path.clone(), 0).unwrap();
        let who = Identity {
            kind: super::super::Kind::User,
            name: "bob".into(),
            groups: vec![],
            display_name: None,
        };
        let (raw, digest) = s.create(Method::Password, who, None, 100, 200).unwrap();
        assert!(s.get(&digest, 150).is_some());
        assert!(s.get(&digest, 200).is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&raw));
        let s2 = SessionStore::open(path, 150).unwrap();
        assert_eq!(s2.active(150), 1);
        s2.remove(&digest).unwrap();
        assert!(s2.get(&digest, 150).is_none());

        // caps: two sessions per owner, four in all
        let s = SessionStore::open(d.path().join("auth/capped.json"), 0)
            .unwrap()
            .with_caps(4, 2);
        let who = |name: &str| Identity {
            kind: super::super::Kind::User,
            name: name.into(),
            groups: vec![],
            display_name: None,
        };
        let new = |name: &str| {
            s.create(Method::Password, who(name), None, 100, 200)
                .unwrap()
                .1
        };
        let a: Vec<[u8; 32]> = (0..3).map(|_| new("a")).collect();
        // a third session of a ends its first, in the same second too
        assert!(s.get(&a[0], 150).is_none());
        assert!(s.get(&a[1], 150).is_some() && s.get(&a[2], 150).is_some());
        let b = new("b");
        let c = new("c");
        assert_eq!(s.active(150), 4);
        // full: the owner with the most sessions (a) loses its oldest, not b or c
        let d1 = new("d");
        assert!(s.get(&a[1], 150).is_none());
        for k in [a[2], b, c, d1] {
            assert!(s.get(&k, 150).is_some());
        }
        // all tied: the oldest session goes
        new("e");
        assert!(s.get(&a[2], 150).is_none());
        assert!(s.get(&b, 150).is_some());
        assert_eq!(s.active(150), 4);
        // the order survives a restart
        let s3 = SessionStore::open(d.path().join("auth/capped.json"), 150)
            .unwrap()
            .with_caps(4, 2);
        s3.create(Method::Password, who("f"), None, 150, 200)
            .unwrap();
        assert!(s3.get(&b, 150).is_none());
        assert!(s3.get(&c, 150).is_some());

        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; sparkles_session=xyz.q; b=2"),
        );
        assert_eq!(cookie(&h, "sparkles_session"), Some("xyz.q"));
        assert_eq!(cookie(&h, "c"), None);
        let m = CookieMode { secure: true };
        assert_eq!(
            m.set("sparkles_session", "v", 60),
            "__Host-sparkles_session=v; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=60"
        );
    }
}
