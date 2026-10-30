//! UI sessions: `<data>/auth/sessions.json` (hashed ids), the session key, signed
//! cookies and CSRF tokens.
//!
//! A session ends at its absolute expiry (`session.ttl` after the login) and, with
//! `session.idle_timeout`, once it has not been used for that long. The time of last use
//! is kept in memory and written with the next write of the store, the hourly prune and
//! at graceful shutdown.

use super::store::{self, parse_rfc3339, rfc3339};
use super::{Identity, crypto};
use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, HeaderValue, header};
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sparkles::access::authority::AuthorityRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// the last request made with the session (as of the last write of the store)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    /// an OIDC login: the provider's session id (`sid`) and subject (`sub`), which a
    /// back-channel logout names
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// creation order (memory only; `created` has whole seconds)
    #[serde(skip)]
    seq: u64,
}

impl SessionRecord {
    pub fn expires_at(&self) -> i64 {
        parse_rfc3339(&self.expires).unwrap_or(0)
    }

    /// When the session was last used, as far as this record knows.
    fn seen_at(&self) -> i64 {
        self.last_seen
            .as_deref()
            .and_then(parse_rfc3339)
            .unwrap_or_else(|| parse_rfc3339(&self.created).unwrap_or(0))
    }

    /// When the session ends: its absolute expiry, or `idle` seconds after its last use
    /// when that comes first.
    pub fn ends_at(&self, idle: Option<i64>) -> i64 {
        self.ends_at_with_seen(idle, None)
    }

    fn ends_at_with_seen(&self, idle: Option<i64>, seen: Option<i64>) -> i64 {
        let abs = self.expires_at();
        match idle {
            Some(i) => abs.min(seen.unwrap_or_else(|| self.seen_at()).saturating_add(i)),
            None => abs,
        }
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
    authority: Arc<AuthorityRegistry>,
    inner: Mutex<HashMap<[u8; 32], SessionRecord>>,
    /// last use of sessions since the last write (Unix seconds)
    seen: Mutex<HashMap<[u8; 32], i64>>,
    /// ID tokens of OIDC sessions (memory only), for RP-initiated logout
    id_tokens: Mutex<HashMap<[u8; 32], String>>,
    next_seq: AtomicU64,
    /// [`MAX_SESSIONS`] and [`MAX_SESSIONS_PER_OWNER`]
    caps: (usize, usize),
}

struct SessionCreation {
    method: Method,
    principal: Identity,
    token_id: Option<String>,
    now: i64,
    expires: i64,
    sid: Option<String>,
    sub: Option<String>,
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
    #[cfg(test)]
    pub fn open(path: PathBuf, now: i64) -> Result<SessionStore> {
        Self::open_with_registry(path, now, Arc::new(AuthorityRegistry::default()))
    }

    pub(super) fn open_with_registry(
        path: PathBuf,
        now: i64,
        authority: Arc<AuthorityRegistry>,
    ) -> Result<SessionStore> {
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
            authority,
            inner: Mutex::new(map),
            seen: Mutex::new(HashMap::new()),
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

    /// Write the store, with the times of last use noted since the last write.
    fn save(&self, map: &mut HashMap<[u8; 32], SessionRecord>) -> Result<()> {
        for (k, t) in std::mem::take(&mut *self.seen.lock()) {
            if let Some(r) = map.get_mut(&k) {
                r.last_seen = Some(rfc3339(t));
            }
        }
        let mut sessions: Vec<SessionRecord> = map.values().cloned().collect();
        sessions.sort_by_key(|s| s.seq);
        let f = SessionFile {
            format: 1,
            sessions,
        };
        store::write_private(&self.path, &serde_json::to_vec_pretty(&f)?)
    }

    /// Start a session; returns the raw session id (the cookie value before signing).
    #[cfg(test)]
    pub fn create(
        &self,
        method: Method,
        principal: Identity,
        token_id: Option<String>,
        now: i64,
        expires: i64,
    ) -> Result<(String, [u8; 32])> {
        self.create_oidc(method, principal, token_id, now, expires, None, None, None)
    }

    /// Create from one original successful authentication generation. The
    /// comparison happens under the shared issue fence, so it fails with
    /// [`AuthorityChanged`](super::store::AuthorityChanged) only when an exclusive
    /// authority update completed after that authentication.
    pub(super) fn create_checked(
        &self,
        original_generation: u64,
        method: Method,
        principal: Identity,
        token_id: Option<String>,
        now: i64,
        expires: i64,
    ) -> Result<(String, [u8; 32])> {
        self.create_session(
            SessionCreation {
                method,
                principal,
                token_id,
                now,
                expires,
                sid: None,
                sub: None,
            },
            Some(original_generation),
        )
    }

    /// Start an OIDC session, remembering the provider's `sid` and `sub` for
    /// back-channel logout. With `original_generation`, the generation whose policy
    /// admitted the identity, it fails like
    /// [`create_checked`](Self::create_checked) when an exclusive authority update
    /// completed since then.
    #[allow(clippy::too_many_arguments)]
    pub fn create_oidc(
        &self,
        method: Method,
        principal: Identity,
        token_id: Option<String>,
        now: i64,
        expires: i64,
        sid: Option<String>,
        sub: Option<String>,
        original_generation: Option<u64>,
    ) -> Result<(String, [u8; 32])> {
        self.create_session(
            SessionCreation {
                method,
                principal,
                token_id,
                now,
                expires,
                sid,
                sub,
            },
            original_generation,
        )
    }

    fn create_session(
        &self,
        input: SessionCreation,
        original_generation: Option<u64>,
    ) -> Result<(String, [u8; 32])> {
        let SessionCreation {
            method,
            principal,
            token_id,
            now,
            expires,
            sid,
            sub,
        } = input;
        let raw = crypto::random_token(32);
        let digest = crypto::sha256(raw.as_bytes());
        let mut rec = SessionRecord {
            id: format!("sha256:{}", crypto::hex(&digest)),
            method,
            principal,
            token_id,
            created: rfc3339(now),
            expires: rfc3339(expires),
            last_seen: None,
            sid,
            sub,
            seq: 0,
        };
        let (max, per_owner) = self.caps;
        // Issuance changes no other principal's rights, so it shares the fence.
        // Sessions it evicts are revoked one by one, like a logout.
        let issue = self.authority.begin_issue();
        if original_generation.is_some_and(|original| original != issue.generation()) {
            return Err(super::store::AuthorityChanged.into());
        }
        rec.seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
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
        let saved = self.save(&mut map);
        drop(map);
        let mut ids = self.id_tokens.lock();
        let mut seen = self.seen.lock();
        for k in ended {
            ids.remove(&k);
            seen.remove(&k);
        }
        drop(ids);
        drop(issue);
        saved?;
        Ok((raw, digest))
    }

    /// A live session: before its absolute expiry and, with an `idle` timeout, used
    /// within it. Using it moves its last use to `now`; the returned record has it.
    pub fn get(&self, digest: &[u8; 32], now: i64, idle: Option<i64>) -> Option<SessionRecord> {
        let mut rec = self.inner.lock().get(digest).cloned()?;
        let mut seen = self.seen.lock();
        if let Some(t) = seen.get(digest) {
            rec.last_seen = Some(rfc3339(*t));
        }
        if rec.ends_at(idle) <= now {
            return None;
        }
        seen.insert(*digest, now);
        rec.last_seen = Some(rfc3339(now));
        Some(rec)
    }

    /// Whether the session is still stored. Ending a session revokes only that
    /// credential: streams bound to it check this instead of the global generation.
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        self.inner.lock().contains_key(digest)
    }

    pub fn remove(&self, digest: &[u8; 32]) -> Result<Option<SessionRecord>> {
        self.id_tokens.lock().remove(digest);
        self.seen.lock().remove(digest);
        let mut map = self.inner.lock();
        let r = map.remove(digest);
        if r.is_some() {
            self.save(&mut map)?;
        }
        Ok(r)
    }

    /// End the OIDC sessions a back-channel logout names: the one with provider session
    /// `sid` (of subject `sub` when both are given), or every session of `sub`. Returns
    /// how many ended.
    pub fn end_oidc(&self, sid: Option<&str>, sub: Option<&str>) -> Result<usize> {
        let mut map = self.inner.lock();
        let gone: Vec<[u8; 32]> = map
            .iter()
            .filter(|(_, s)| {
                s.method == Method::Oidc
                    && match (sid, sub) {
                        (Some(i), u) => {
                            s.sid.as_deref() == Some(i)
                                && u.is_none_or(|u| s.sub.as_deref() == Some(u))
                        }
                        (None, Some(u)) => s.sub.as_deref() == Some(u),
                        (None, None) => false,
                    }
            })
            .map(|(k, _)| *k)
            .collect();
        for k in &gone {
            map.remove(k);
        }
        if !gone.is_empty() {
            self.save(&mut map)?;
        }
        drop(map);
        let (mut ids, mut seen) = (self.id_tokens.lock(), self.seen.lock());
        for k in &gone {
            ids.remove(k);
            seen.remove(k);
        }
        Ok(gone.len())
    }

    /// Record the groups an identity has now in its sessions (an OIDC login refreshed
    /// them). Returns how many sessions changed.
    pub fn refresh_groups(&self, who: &Identity) -> Result<usize> {
        if !self.inner.lock().values().any(|session| {
            session.principal.kind == who.kind
                && session.principal.name == who.name
                && session.principal.groups != who.groups
        }) {
            return Ok(0);
        }
        let _update = self.authority.begin_update();
        let mut map = self.inner.lock();
        let mut n = 0;
        for s in map.values_mut() {
            if s.principal.kind == who.kind
                && s.principal.name == who.name
                && s.principal.groups != who.groups
            {
                s.principal.groups = who.groups.clone();
                n += 1;
            }
        }
        if n > 0 {
            self.save(&mut map)?;
        }
        Ok(n)
    }

    pub fn set_id_token(&self, digest: [u8; 32], t: String) {
        self.id_tokens.lock().insert(digest, t);
    }

    pub fn id_token(&self, digest: &[u8; 32]) -> Option<String> {
        self.id_tokens.lock().get(digest).cloned()
    }

    /// Drop expired and idle sessions, and write the times of last use (hourly).
    /// Lookups already refuse these sessions, so pruning them changes no rights
    /// and leaves the authority generation unchanged.
    pub fn prune(&self, now: i64, idle: Option<i64>) -> Result<()> {
        let mut map = self.inner.lock();
        let before = map.len();
        let seen = self.seen.lock().clone();
        map.retain(|k, s| s.ends_at_with_seen(idle, seen.get(k).copied()) > now);
        if map.len() != before || !seen.is_empty() {
            self.save(&mut map)?;
        }
        Ok(())
    }

    /// Write the times of last use not yet written (graceful shutdown).
    pub fn flush(&self) -> Result<()> {
        if self.seen.lock().is_empty() {
            return Ok(());
        }
        let mut map = self.inner.lock();
        self.save(&mut map)
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
    fn pruning_never_moves_the_generation_and_removes_only_ended_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let registry = Arc::new(AuthorityRegistry::default());
        let sessions = SessionStore::open_with_registry(path.clone(), 0, registry.clone()).unwrap();
        let initial = registry.generation();
        sessions.prune(100, Some(10)).unwrap();
        assert_eq!(registry.generation(), initial);
        assert!(!path.exists());
        let owner = Identity {
            kind: super::super::Kind::User,
            name: "idle-owner".into(),
            groups: vec![],
            display_name: None,
        };
        let (_, digest) = sessions
            .create(Method::Password, owner, None, 100, 1000)
            .unwrap();
        let accepted = registry.generation();
        let before = std::fs::read(&path).unwrap();
        sessions.prune(105, Some(10)).unwrap();
        assert_eq!(registry.generation(), accepted);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(sessions.get(&digest, 109, Some(10)).is_some());
        // The unsaved last use keeps this session live past created+idle.
        sessions.prune(111, Some(10)).unwrap();
        assert_eq!(registry.generation(), accepted);
        let saved = std::fs::read(&path).unwrap();
        assert_ne!(saved, before);
        let reopened = SessionStore::open(path.clone(), 111).unwrap();
        assert!(reopened.get(&digest, 118, Some(10)).is_some());
        drop(reopened);
        sessions.prune(119, Some(10)).unwrap();
        assert_eq!(registry.generation(), accepted);
        assert!(!sessions.contains(&digest));
        assert!(sessions.get(&digest, 119, Some(10)).is_none());
        let reopened = SessionStore::open(path, 119).unwrap();
        assert_eq!(reopened.active(119), 0);
    }

    #[test]
    fn checked_issuance_rejects_stale_or_changing_authentication_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let registry = Arc::new(AuthorityRegistry::default());
        let sessions = SessionStore::open_with_registry(path.clone(), 0, registry.clone())
            .unwrap()
            .with_caps(1, 1);
        let owner = || Identity {
            kind: super::super::Kind::User,
            name: "checked-owner".into(),
            groups: vec![],
            display_name: None,
        };
        let original = registry.generation();
        let (_, kept) = sessions
            .create_checked(original, Method::Password, owner(), None, 100, 200)
            .unwrap();
        assert_eq!(
            registry.generation(),
            original,
            "issuance must not invalidate other authentications"
        );
        let before = std::fs::read(&path).unwrap();
        let next_sequence = sessions.next_seq.load(Ordering::Relaxed);

        // A real authority update invalidates the original generation.
        // At cap=1 an accidental insertion would also evict the existing session.
        drop(registry.begin_update());
        for rejected in [original, registry.generation() + 1] {
            let error = sessions
                .create_checked(rejected, Method::Password, owner(), None, 100, 200)
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<super::super::store::AuthorityChanged>()
                    .is_some()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(sessions.next_seq.load(Ordering::Relaxed), next_sequence);
            assert!(sessions.get(&kept, 100, None).is_some());
            assert_eq!(sessions.active(100), 1);
        }
        let current = registry.generation();
        let (_, replacement) = sessions
            .create_checked(current, Method::Password, owner(), None, 100, 200)
            .unwrap();
        assert!(sessions.get(&kept, 100, None).is_none());
        assert!(sessions.get(&replacement, 100, None).is_some());
        let reopened = SessionStore::open(path, 100).unwrap();
        assert!(reopened.get(&replacement, 100, None).is_some());
    }

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
        assert!(s.get(&digest, 150, None).is_some());
        assert!(s.get(&digest, 200, None).is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&raw));
        let s2 = SessionStore::open(path, 150).unwrap();
        assert_eq!(s2.active(150), 1);
        s2.remove(&digest).unwrap();
        assert!(s2.get(&digest, 150, None).is_none());

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
        assert!(s.get(&a[0], 150, None).is_none());
        assert!(s.get(&a[1], 150, None).is_some() && s.get(&a[2], 150, None).is_some());
        let b = new("b");
        let c = new("c");
        assert_eq!(s.active(150), 4);
        // full: the owner with the most sessions (a) loses its oldest, not b or c
        let d1 = new("d");
        assert!(s.get(&a[1], 150, None).is_none());
        for k in [a[2], b, c, d1] {
            assert!(s.get(&k, 150, None).is_some());
        }
        // all tied: the oldest session goes
        new("e");
        assert!(s.get(&a[2], 150, None).is_none());
        assert!(s.get(&b, 150, None).is_some());
        assert_eq!(s.active(150), 4);
        // the order survives a restart
        let s3 = SessionStore::open(d.path().join("auth/capped.json"), 150)
            .unwrap()
            .with_caps(4, 2);
        s3.create(Method::Password, who("f"), None, 150, 200)
            .unwrap();
        assert!(s3.get(&b, 150, None).is_none());
        assert!(s3.get(&c, 150, None).is_some());

        // idle sessions end; a use keeps one alive, and the last use is written
        let path = d.path().join("auth/idle.json");
        let s = SessionStore::open(path.clone(), 0).unwrap();
        let (_, k) = s
            .create(Method::Password, who("i"), None, 1000, 9000)
            .unwrap();
        assert!(s.get(&k, 1500, Some(600)).is_some());
        assert!(s.get(&k, 2000, Some(600)).is_some());
        assert_eq!(s.get(&k, 2000, Some(600)).unwrap().ends_at(Some(600)), 2600);
        assert!(s.get(&k, 2601, Some(600)).is_none());
        s.flush().unwrap();
        let s2 = SessionStore::open(path.clone(), 3000).unwrap();
        assert!(s2.get(&k, 3500, Some(600)).is_none());
        assert!(s2.get(&k, 3500, Some(2000)).is_some());
        s2.prune(4600, Some(1000)).unwrap();
        assert_eq!(s2.active(4600), 0);
        // without an idle timeout only the absolute expiry counts
        let (_, k) = s
            .create(Method::Password, who("j"), None, 1000, 9000)
            .unwrap();
        assert!(s.get(&k, 8999, None).is_some());
        assert!(s.get(&k, 9000, None).is_none());

        // back-channel logout by sid or by sub
        let s = SessionStore::open(d.path().join("auth/oidc.json"), 0).unwrap();
        let oidc = |sid: &str, sub: &str| {
            s.create_oidc(
                Method::Oidc,
                who(sub),
                None,
                100,
                200,
                Some(sid.into()),
                Some(sub.into()),
                None,
            )
            .unwrap()
            .1
        };
        let (x1, x2, y1) = (oidc("s1", "x"), oidc("s2", "x"), oidc("s3", "y"));
        assert_eq!(s.end_oidc(Some("s1"), Some("y")).unwrap(), 0);
        assert_eq!(s.end_oidc(Some("s1"), None).unwrap(), 1);
        assert!(s.get(&x1, 150, None).is_none() && s.get(&x2, 150, None).is_some());
        assert_eq!(s.end_oidc(None, Some("x")).unwrap(), 1);
        assert!(s.get(&x2, 150, None).is_none() && s.get(&y1, 150, None).is_some());
        assert_eq!(s.end_oidc(None, None).unwrap(), 0);

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
