//! The loaded policy, the auth state of the server, and authentication of requests.

use super::config::{self, FileConfig};
use super::proxy::{Peer, ProxySettings};
use super::session::{CookieMode, Keys, Method, SessionStore};
use super::tokens::{TokenRecord, TokenStore};
use super::{
    Access, Grants, Identity, Kind, Level, Principal, PrincipalInfo, Scheme, ServerPerm, crypto,
    store,
};
use crate::ratelimit::{Admission, ClientKey};
use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use axum::http::{HeaderMap, header};
use base64::Engine;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Entries of the verified-credential cache, and how long they stay valid.
const CACHE_MAX: usize = 10_000;
const CACHE_TTL: Duration = Duration::from_secs(300);
/// How long a password verification may wait for a permit before `busy`.
const PERMIT_WAIT: Duration = Duration::from_secs(5);
/// Password verifications that may wait for a permit, per permit; more are refused at
/// once (`busy`) instead of queueing without bound.
const QUEUE_PER_PERMIT: usize = 4;
/// Password verifications (running and waiting) of one network (an IPv4 address, an
/// IPv6 /48) at most, at least this many: max(2, permits). Beyond it `busy`, so that one
/// network cannot fill the whole queue.
const MIN_PER_CLIENT: usize = 2;

/// Named limits of [`Auth::throttle`]: tokens minted per owner, device logins started per
/// client network, failed user-code lookups per client network and per owner.
pub const MINT: &str = "mint";
pub const DEVICE: &str = "device";
pub const DEVICE_CODE: &str = "device-code";
/// Clients the throttle tracks at most.
const THROTTLE_KEYS: usize = 10_000;

/// `count` per minute, `burst` at once.
fn per_minute(count: u32, burst: u32) -> crate::ratelimit::Limit {
    crate::ratelimit::Limit {
        rate: Some(crate::ratelimit::Rate {
            count,
            period: Duration::from_secs(60),
        }),
        burst: Some(burst),
        ..Default::default()
    }
}

/// The throttle's limits under `policy`: its mint rate; 20 device logins per address,
/// then two a minute (the pending grants of one address stay far below the server's cap);
/// 20 unknown user codes per client network and per owner, then two a minute.
fn throttle_config(policy: &Policy) -> crate::ratelimit::Config {
    let mut c = crate::ratelimit::Config {
        max_keys: Some(THROTTLE_KEYS),
        ..Default::default()
    };
    c.named
        .insert(MINT, (policy.mint_rate.clone(), "tokens minted"));
    c.named
        .insert(DEVICE, (per_minute(2, 20), "device logins started"));
    c.named
        .insert(DEVICE_CODE, (per_minute(2, 20), "unknown codes"));
    c
}

/// A static token from the configuration.
struct StaticToken {
    id: String,
    expires: Option<i64>,
    grants: Grants,
}

struct UserEntry {
    /// argon2id PHC string
    password: String,
    grants: Grants,
}

/// Admission and role mapping of OIDC and proxy identities.
#[derive(Default)]
struct External {
    allowed_users: HashSet<String>,
    allowed_groups: HashSet<String>,
    default_roles: Vec<String>,
    group_roles: BTreeMap<String, Vec<String>>,
    user_roles: BTreeMap<String, Vec<String>>,
}

/// One validated configuration, swapped as a whole on reload.
pub struct Policy {
    pub realm: String,
    /// `server.public_url` without a trailing slash
    pub public_url: Option<String>,
    anonymous: Grants,
    users: HashMap<String, UserEntry>,
    static_tokens: HashMap<[u8; 32], StaticToken>,
    /// id, name, expiry and grants summary of each static token, for listings
    pub static_list: Vec<(String, String, Option<i64>, String)>,
    roles: HashMap<String, Grants>,
    /// the protections of triples, shared by every principal's grants
    protections: Option<Arc<super::Protections>>,
    external: External,
    pub default_ttl: i64,
    pub max_ttl: i64,
    /// unexpired minted tokens per owner at most
    pub max_tokens_per_owner: usize,
    /// tokens an owner may mint
    pub mint_rate: crate::ratelimit::Limit,
    pub session_ttl: i64,
    pub oidc: Option<config::OidcCfg>,
    pub proxy: Option<ProxySettings>,
    pub cors_origins: Vec<String>,
    /// verified in place of the hash of an unknown user, so timing reveals no names
    dummy_hash: String,
    pub counts: (usize, usize, usize),
}

fn grants_of(
    datasets: &BTreeMap<String, Level>,
    server: &[ServerPerm],
    restricted: &[config::GrantCfg],
    roles: &[String],
    role_grants: &HashMap<String, Grants>,
    protections: &Option<Arc<super::Protections>>,
) -> Grants {
    let mut g = Grants {
        datasets: datasets.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        server: Vec::new(),
        restricted: restricted
            .iter()
            .map(config::GrantCfg::restricted)
            .collect(),
        roles: roles.to_vec(),
        protections: protections.clone(),
    };
    for s in server {
        if !g.server.contains(s) {
            g.server.push(*s);
        }
    }
    for r in roles {
        if let Some(rg) = role_grants.get(r) {
            g.extend(rg);
        }
    }
    g
}

fn summarize(g: &Grants) -> String {
    let mut parts: Vec<String> = g
        .datasets
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str()))
        .collect();
    parts.extend(
        g.restricted
            .iter()
            .map(|r| format!("{}={} (limited)", r.dataset, r.level.as_str())),
    );
    parts.extend(g.server.iter().map(|s| s.as_str().to_string()));
    parts.join(", ")
}

impl Policy {
    pub fn build(cfg: &FileConfig) -> Result<Policy> {
        let protections = (!cfg.protections.is_empty()).then(|| {
            Arc::new(super::Protections {
                list: cfg
                    .protections
                    .iter()
                    .map(|p| (p.dataset.clone(), Arc::new(p.protection())))
                    .collect(),
                limits: sparkles::access::Limits {
                    max_hidden: cfg.protection_limits.max_hidden_quads,
                    max_pattern_rows: cfg.protection_limits.max_pattern_rows,
                },
            })
        });
        let roles: HashMap<String, Grants> = cfg
            .roles
            .iter()
            .map(|(n, r)| {
                (
                    n.clone(),
                    grants_of(
                        &r.datasets,
                        &r.server,
                        &r.grants,
                        &[],
                        &HashMap::new(),
                        &protections,
                    ),
                )
            })
            .collect();
        let anonymous = grants_of(
            &cfg.anonymous.datasets,
            &cfg.anonymous.server,
            &cfg.anonymous.grants,
            &[],
            &roles,
            &protections,
        );
        let mut users = HashMap::new();
        for u in &cfg.users {
            users.insert(
                u.name.clone(),
                UserEntry {
                    password: u.password.clone(),
                    grants: grants_of(
                        &u.datasets,
                        &u.server,
                        &u.grants,
                        &u.roles,
                        &roles,
                        &protections,
                    ),
                },
            );
        }
        let mut static_tokens = HashMap::new();
        let mut static_list = Vec::new();
        for t in &cfg.tokens {
            let grants = grants_of(
                &t.datasets,
                &t.server,
                &t.grants,
                &t.roles,
                &roles,
                &protections,
            );
            let digest = config::parse_token_hash(&t.hash).context("token hash")?;
            let expires = t
                .expires
                .as_deref()
                .map(config::parse_rfc3339)
                .transpose()?;
            let id = format!("cfg-{}", t.name);
            static_list.push((id.clone(), t.name.clone(), expires, summarize(&grants)));
            static_tokens.insert(
                digest,
                StaticToken {
                    id,
                    expires,
                    grants,
                },
            );
        }
        let e = &cfg.external;
        let external = External {
            allowed_users: e.allowed_users.iter().cloned().collect(),
            allowed_groups: e.allowed_groups.iter().cloned().collect(),
            default_roles: e.default_roles.clone(),
            group_roles: e.group_roles.clone(),
            user_roles: e.user_roles.clone(),
        };
        // the dummy uses the parameters of a configured password, so an unknown name
        // costs as much as a known one
        let (m, t, p) = cfg
            .users
            .iter()
            .find_map(|u| config::argon2id_params(&u.password))
            .unwrap_or((config::OWASP_M, config::OWASP_T, config::OWASP_P));
        let dummy_hash = hash_password_with(&crypto::random_token(24), m, t, p)?;
        Ok(Policy {
            realm: cfg.realm.clone(),
            public_url: cfg
                .server
                .public_url
                .as_ref()
                .map(|u| u.trim_end_matches('/').to_string()),
            anonymous,
            users,
            static_tokens,
            static_list,
            roles,
            protections,
            external,
            default_ttl: config::parse_duration(&cfg.tokens_policy.default_ttl)?,
            max_ttl: config::parse_duration(&cfg.tokens_policy.max_ttl)?,
            max_tokens_per_owner: cfg.tokens_policy.max_active_per_owner,
            mint_rate: config::parse_mint_rate(&cfg.tokens_policy.mint_rate)?,
            session_ttl: config::parse_duration(&cfg.session.ttl)?,
            oidc: cfg.oidc.clone(),
            proxy: cfg.proxy.as_ref().map(ProxySettings::from_config),
            cors_origins: cfg.cors.origins.clone(),
            dummy_hash,
            counts: (cfg.users.len(), cfg.tokens.len(), cfg.roles.len()),
        })
    }

    pub fn has_users(&self) -> bool {
        !self.users.is_empty()
    }

    /// Admission of an OIDC or proxy identity: with both lists empty everyone is
    /// admitted; otherwise the name or one of the groups must be listed.
    pub fn admitted(&self, name: &str, groups: &[String]) -> bool {
        let x = &self.external;
        (x.allowed_users.is_empty() && x.allowed_groups.is_empty())
            || x.allowed_users.contains(name)
            || groups.iter().any(|g| x.allowed_groups.contains(g))
    }

    /// The grants of an identity under this policy; `None` when it no longer exists or
    /// is no longer admitted.
    pub fn identity_grants(&self, who: &Identity) -> Option<Grants> {
        match who.kind {
            Kind::User => self.users.get(&who.name).map(|u| u.grants.clone()),
            Kind::Oidc | Kind::Proxy => {
                if !self.admitted(&who.name, &who.groups) {
                    return None;
                }
                let x = &self.external;
                let mut roles: Vec<&String> = x.default_roles.iter().collect();
                for g in &who.groups {
                    roles.extend(x.group_roles.get(g).into_iter().flatten());
                }
                roles.extend(x.user_roles.get(&who.name).into_iter().flatten());
                let mut grants = Grants {
                    protections: self.protections.clone(),
                    ..Default::default()
                };
                for r in roles {
                    if !grants.roles.contains(r) {
                        grants.roles.push(r.clone());
                    }
                    if let Some(rg) = self.roles.get(r) {
                        grants.extend(rg);
                    }
                }
                Some(grants)
            }
            _ => None,
        }
    }
}

/// An argon2id PHC string for `password` with explicit parameters.
pub fn hash_password_with(password: &str, m: u32, t: u32, p: u32) -> Result<String> {
    use argon2::password_hash::PasswordHasher;
    let params = argon2::Params::new(m, t, p, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    Ok(a.hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_string())
}

/// An argon2id PHC string with the OWASP parameters (`sparkles auth hash`).
pub fn hash_password(password: &str) -> Result<String> {
    hash_password_with(password, config::OWASP_M, config::OWASP_T, config::OWASP_P)
}

fn verify_password(password: &[u8], phc: &str) -> bool {
    use argon2::password_hash::PasswordVerifier;
    argon2::Argon2::default()
        .verify_password(password, phc)
        .is_ok()
}

/// A well-formed token: `spk_` + 43 base64url characters.
pub fn well_formed_token(t: &str) -> bool {
    t.len() == 47
        && t.starts_with("spk_")
        && t[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A new random token (`spk_` + 43 base64url characters of 32 random bytes).
pub fn new_token() -> String {
    format!("spk_{}", crypto::random_token(32))
}

/// `sha256:<hex>` of a token string.
pub fn token_hash(token: &str) -> String {
    format!("sha256:{}", crypto::sha256_hex(token.as_bytes()))
}

/// Why authentication failed (metric label `reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Malformed,
    Invalid,
    Expired,
    Busy,
    State,
    Idp,
    NotAllowed,
    /// not checked: the client address has no authentication failures left
    Limited,
}

impl Failure {
    pub const ALL: [Failure; 8] = [
        Failure::Malformed,
        Failure::Invalid,
        Failure::Expired,
        Failure::Busy,
        Failure::State,
        Failure::Idp,
        Failure::NotAllowed,
        Failure::Limited,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Failure::Malformed => "malformed",
            Failure::Invalid => "invalid",
            Failure::Expired => "expired",
            Failure::Busy => "busy",
            Failure::State => "state",
            Failure::Idp => "idp",
            Failure::NotAllowed => "not_allowed",
            Failure::Limited => "limited",
        }
    }
}

/// Metric label `scheme` of failures.
pub const FAILURE_SCHEMES: [&str; 5] = ["basic", "bearer", "session", "oidc", "proxy"];

/// A failed authentication: the scheme tried (`FAILURE_SCHEMES`) and why it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthError {
    pub scheme: &'static str,
    pub failure: Failure,
}

/// The result of authenticating a request.
pub struct Authenticated {
    pub principal: Principal,
    /// the request carried an invalid session cookie: the response clears it
    pub clear_cookie: bool,
}

/// Counters of the auth layer (closed label sets; no principal label).
#[derive(Default)]
pub struct AuthMetrics {
    /// `[scheme][reason]` (`FAILURE_SCHEMES` × `Failure::ALL`)
    pub failures: [[AtomicU64; 8]; 5],
    /// `routes::Denied::ALL`
    pub denied: [AtomicU64; 6],
    /// `[method][result]`: oidc, password, token × ok, denied, error
    pub logins: [[AtomicU64; 3]; 3],
    /// api, ui, cli-loopback, cli-device
    pub minted: [AtomicU64; 4],
    pub revoked: AtomicU64,
    pub password_verifications: AtomicU64,
    pub untrusted_proxy_headers: AtomicU64,
    /// ok, error
    pub reloads: [AtomicU64; 2],
}

pub const MINT_VIA: [&str; 4] = ["api", "ui", "cli-loopback", "cli-device"];

/// Verified Basic credentials and verifications in progress, keyed by
/// `HMAC(k, user ‖ 0 ‖ password)`; one lock, so a request never misses both.
#[derive(Default)]
struct Creds {
    cache: HashMap<[u8; 32], (Principal, Instant)>,
    inflight: HashMap<[u8; 32], Inflight>,
}

/// A password verification in progress, shared by concurrent requests.
type Inflight = Arc<tokio::sync::OnceCell<Option<Principal>>>;

/// The permits of argon2 verifications and the bounded queue for them.
struct Argon {
    permits: tokio::sync::Semaphore,
    size: usize,
    waiting: AtomicUsize,
    /// verifications running and waiting, per client network
    clients: Mutex<HashMap<ClientKey, usize>>,
    per_client: usize,
}

/// A client network's place among the password verifications, left when dropped.
struct ClientSlot<'a> {
    clients: &'a Mutex<HashMap<ClientKey, usize>>,
    key: ClientKey,
}

impl<'a> ClientSlot<'a> {
    fn enter(
        clients: &'a Mutex<HashMap<ClientKey, usize>>,
        key: &ClientKey,
        max: usize,
    ) -> Option<ClientSlot<'a>> {
        let mut m = clients.lock();
        let n = m.entry(key.clone()).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(ClientSlot {
            clients,
            key: key.clone(),
        })
    }
}

impl Drop for ClientSlot<'_> {
    fn drop(&mut self) {
        let mut m = self.clients.lock();
        if let Some(n) = m.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// A place in the queue of password verifications, left when dropped (also when the
/// request is cancelled).
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    fn enter(n: &'a AtomicUsize, max: usize) -> Option<Waiting<'a>> {
        n.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |w| {
            (w < max).then_some(w + 1)
        })
        .ok()
        .map(|_| Waiting(n))
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The auth state of a server: the current policy, the token and session stores, the
/// login flows in progress and the password machinery.
pub struct Auth {
    path: Option<PathBuf>,
    policy: ArcSwap<Policy>,
    creds: Mutex<Creds>,
    argon: Argon,
    /// the auth layer's own limits ([`MINT`], [`DEVICE`], [`DEVICE_CODE`]) on the
    /// server's rate-limit machinery
    pub throttle: crate::ratelimit::RateLimiter,
    hmac_key: Vec<u8>,
    pub tokens: TokenStore,
    pub sessions: SessionStore,
    pub keys: Keys,
    pub oidc: super::oidc::OidcState,
    pub cli: super::grants::CliGrants,
    pub metrics: AuthMetrics,
    /// seconds added to the clock (tests)
    clock_offset: AtomicI64,
    untrusted_warned: Mutex<Option<Instant>>,
}

impl Auth {
    /// Load and validate `path`, and open the stores under `<data>/auth/`; returns the
    /// warnings to log.
    pub fn open(path: &Path, data_dir: &Path) -> Result<(Auth, Vec<String>)> {
        let (cfg, warnings) = FileConfig::load(path)?;
        let mut auth = Auth::from_config(&cfg, data_dir)?;
        auth.path = Some(path.to_path_buf());
        Ok((auth, warnings))
    }

    pub fn from_config(cfg: &FileConfig, data_dir: &Path) -> Result<Auth> {
        let dir = data_dir.join("auth");
        store::private_dir(&dir)?;
        let key_file = cfg
            .session
            .key_file
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.join("session.key"));
        let permits = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .max(1);
        let policy = Policy::build(cfg)?;
        let now = store::unix_now();
        let oidc = super::oidc::OidcState::new(&policy)?;
        let throttle = crate::ratelimit::RateLimiter::new(&throttle_config(&policy))
            .map_err(anyhow::Error::msg)?;
        Ok(Auth {
            path: None,
            policy: ArcSwap::from_pointee(policy),
            creds: Mutex::new(Creds::default()),
            argon: Argon {
                permits: tokio::sync::Semaphore::new(permits),
                size: permits,
                waiting: AtomicUsize::new(0),
                clients: Mutex::new(HashMap::new()),
                per_client: permits.max(MIN_PER_CLIENT),
            },
            throttle,
            hmac_key: crypto::random_bytes(32),
            tokens: TokenStore::open(dir.join("tokens.json"))?,
            sessions: SessionStore::open(dir.join("sessions.json"), now)?,
            keys: Keys::load_or_create(&key_file)?,
            oidc,
            cli: super::grants::CliGrants::default(),
            metrics: AuthMetrics::default(),
            clock_offset: AtomicI64::new(0),
            untrusted_warned: Mutex::new(None),
        })
    }

    pub fn policy(&self) -> Arc<Policy> {
        self.policy.load_full()
    }

    /// Unix seconds (with the test clock offset).
    pub fn now(&self) -> i64 {
        store::unix_now() + self.clock_offset.load(Ordering::Relaxed)
    }

    /// Password verifications running and waiting for a permit.
    pub fn argon_load(&self) -> (usize, usize) {
        let a = &self.argon;
        (
            a.size.saturating_sub(a.permits.available_permits()),
            a.waiting.load(Ordering::Relaxed),
        )
    }

    /// Take every verification permit (tests: verifications queue until it is dropped).
    #[cfg(test)]
    pub fn hold_verifications(&self) -> tokio::sync::SemaphorePermit<'_> {
        let a = &self.argon;
        a.permits.try_acquire_many(a.size as u32).unwrap()
    }

    /// Password verifications that may wait for a permit.
    #[cfg(test)]
    pub fn verification_queue(&self) -> (usize, usize) {
        (self.argon.size, self.argon.size * QUEUE_PER_PERMIT)
    }

    /// Password verifications one client network may have running and waiting.
    #[cfg(test)]
    pub fn verifications_per_client(&self) -> usize {
        self.argon.per_client
    }

    /// Move the clock forward (tests).
    #[cfg(test)]
    pub fn advance(&self, secs: i64) {
        self.clock_offset.fetch_add(secs, Ordering::Relaxed);
    }

    /// Re-read the configuration file. On error the old policy stays.
    pub fn reload(&self) -> Result<Vec<String>> {
        let r = (|| {
            let path = self
                .path
                .as_ref()
                .context("no configuration file to reload")?;
            let (cfg, warnings) = FileConfig::load(path)?;
            let p = Policy::build(&cfg)?;
            self.oidc.reconfigure(&p)?;
            // the throttle keeps its clients' state
            self.throttle
                .reload(&throttle_config(&p))
                .map_err(anyhow::Error::msg)?;
            self.policy.store(Arc::new(p));
            self.creds.lock().cache.clear();
            Ok(warnings)
        })();
        self.metrics.reloads[usize::from(r.is_err())].fetch_add(1, Ordering::Relaxed);
        if r.is_ok() {
            tracing::info!(target: "sparkles::audit", event = "auth_reloaded");
        }
        r
    }

    pub fn count_failure(&self, e: AuthError) {
        let s = FAILURE_SCHEMES
            .iter()
            .position(|x| *x == e.scheme)
            .unwrap_or(1);
        let r = Failure::ALL
            .iter()
            .position(|f| *f == e.failure)
            .unwrap_or(0);
        self.metrics.failures[s][r].fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_login(&self, method: Method, result: &str) {
        let m = match method {
            Method::Oidc => 0,
            Method::Password => 1,
            Method::Token => 2,
        };
        let r = match result {
            "ok" => 0,
            "denied" => 1,
            _ => 2,
        };
        self.metrics.logins[m][r].fetch_add(1, Ordering::Relaxed);
    }

    /// Cookie attributes for this request: `Secure` and `__Host-` when the public URL,
    /// or else the proxy's `X-Forwarded-Proto`, says https.
    pub fn cookie_mode(&self, h: &HeaderMap) -> CookieMode {
        let secure = match &self.policy.load().public_url {
            Some(u) => u.starts_with("https://"),
            None => h
                .get("x-forwarded-proto")
                .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"https")),
        };
        CookieMode { secure }
    }

    /// The anonymous principal of the current policy.
    pub fn anonymous_principal(&self) -> Principal {
        self.anonymous(&self.policy.load())
    }

    fn anonymous(&self, policy: &Policy) -> Principal {
        Principal::new(
            Kind::Anonymous,
            "anonymous",
            Scheme::None,
            Access::of(policy.anonymous.clone()),
        )
    }

    /// The first applicable source wins: `Authorization`, the session cookie, trusted
    /// proxy headers, anonymous. Invalid `Authorization` is an error, never anonymous;
    /// an invalid session cookie is ignored (and cleared).
    ///
    /// `admission`: the request's standing under the pre-authentication limit, charged
    /// before a password is verified; `client`: the network of the client, whose
    /// password verifications are capped.
    pub async fn authenticate(
        &self,
        h: &HeaderMap,
        peer: Option<&Peer>,
        admission: Option<&Admission>,
        client: Option<&ClientKey>,
    ) -> Result<Authenticated, AuthError> {
        let policy = self.policy.load_full();
        let ok = |principal| Authenticated {
            principal,
            clear_cookie: false,
        };
        if let Some(v) = h.get(header::AUTHORIZATION) {
            return self
                .authorization(&policy, v, admission, client)
                .await
                .map(ok);
        }
        let mut clear_cookie = false;
        let mode = self.cookie_mode(h);
        if let Some(c) = super::session::cookie(h, &mode.name(super::SESSION_COOKIE)) {
            match self.session_principal(&policy, c) {
                Some(p) => return Ok(ok(p)),
                None => clear_cookie = true,
            }
        }
        if let Some(px) = &policy.proxy
            && px.has_headers(h)
        {
            if px.trusted.trusts(peer) {
                if let Some((name, groups)) = px.identity(h) {
                    return match self.proxy_principal(&policy, &name, groups) {
                        Some(p) => Ok(Authenticated {
                            principal: p,
                            clear_cookie,
                        }),
                        None => Err(AuthError {
                            scheme: "proxy",
                            failure: Failure::NotAllowed,
                        }),
                    };
                }
            } else {
                self.metrics
                    .untrusted_proxy_headers
                    .fetch_add(1, Ordering::Relaxed);
                let mut last = self.untrusted_warned.lock();
                if last.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                    *last = Some(Instant::now());
                    let from = match peer {
                        Some(Peer::Tcp(a)) => a.ip().to_string(),
                        Some(Peer::Unix) => "the Unix socket".into(),
                        None => "an unknown peer".into(),
                    };
                    tracing::warn!("ignored proxy identity headers from untrusted {from}");
                }
            }
        }
        Ok(Authenticated {
            principal: self.anonymous(&policy),
            clear_cookie,
        })
    }

    async fn authorization(
        &self,
        policy: &Policy,
        v: &axum::http::HeaderValue,
        admission: Option<&Admission>,
        client: Option<&ClientKey>,
    ) -> Result<Principal, AuthError> {
        let malformed = |scheme| AuthError {
            scheme,
            failure: Failure::Malformed,
        };
        let v = v.to_str().map_err(|_| malformed("bearer"))?;
        let (scheme, rest) = v.trim().split_once(' ').unwrap_or((v.trim(), ""));
        let rest = rest.trim();
        if scheme.eq_ignore_ascii_case("bearer") {
            if rest.is_empty() {
                return Err(malformed("bearer"));
            }
            return self.token_principal(policy, rest, Scheme::Bearer);
        }
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(malformed("bearer"));
        }
        let decoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(rest)
                .map_err(|_| malformed("basic"))?,
        );
        let text = std::str::from_utf8(&decoded).map_err(|_| malformed("basic"))?;
        let (user, password) = text.split_once(':').ok_or(malformed("basic"))?;
        if password.starts_with("spk_") {
            // Basic-only clients carry a token as the password; the user is ignored
            return self.token_principal(policy, password, Scheme::Basic);
        }
        self.password(policy, user, password, admission, client)
            .await
    }

    /// A static or minted token.
    pub fn token_principal(
        &self,
        policy: &Policy,
        token: &str,
        scheme: Scheme,
    ) -> Result<Principal, AuthError> {
        let err = |failure| AuthError {
            scheme: if scheme == Scheme::Basic {
                "basic"
            } else {
                "bearer"
            },
            failure,
        };
        if !well_formed_token(token) {
            return Err(err(Failure::Invalid));
        }
        let digest = crypto::sha256(token.as_bytes());
        let now = self.now();
        if let Some(t) = policy.static_tokens.get(&digest) {
            if t.expires.is_some_and(|x| x <= now) {
                return Err(err(Failure::Expired));
            }
            return Ok(
                Principal::new(Kind::Token, &t.id, scheme, Access::of(t.grants.clone())).with_info(
                    PrincipalInfo {
                        token_id: Some(t.id.clone()),
                        static_token: true,
                        expires: t.expires,
                        ..Default::default()
                    },
                ),
            );
        }
        let rec = self
            .tokens
            .by_digest(&digest)
            .ok_or(err(Failure::Invalid))?;
        if rec.expires_at() <= now {
            return Err(err(Failure::Expired));
        }
        let access = self
            .token_access(policy, &rec, now, 0)
            .ok_or(err(Failure::Invalid))?;
        self.tokens.touch(&rec.id, now);
        Ok(minted(rec, scheme, access))
    }

    /// Effective permissions of a minted token: its scope ∩ its parent's (a chain), or
    /// ∩ its owner's grants under the current policy. `None`: no longer valid.
    pub fn token_access(
        &self,
        policy: &Policy,
        rec: &TokenRecord,
        now: i64,
        depth: usize,
    ) -> Option<Access> {
        if depth >= super::tokens::MAX_CHAIN || rec.expires_at() <= now {
            return None;
        }
        let mut access = match &rec.parent {
            Some(pid) => {
                let parent = self.tokens.get(pid)?;
                self.token_access(policy, &parent, now, depth + 1)?
            }
            None => Access::of(policy.identity_grants(&rec.owner)?),
        };
        access.scopes.push(rec.scope.clone());
        Some(access)
    }

    fn proxy_principal(
        &self,
        policy: &Policy,
        name: &str,
        groups: Vec<String>,
    ) -> Option<Principal> {
        let who = Identity {
            kind: Kind::Proxy,
            name: name.to_string(),
            groups,
            display_name: None,
        };
        let grants = policy.identity_grants(&who)?;
        let csrf = self.keys.csrf(&format!("proxy:{name}"));
        Some(
            Principal::new(Kind::Proxy, name, Scheme::Proxy, Access::of(grants)).with_info(
                PrincipalInfo {
                    owner: Some(who),
                    csrf: Some(csrf),
                    ..Default::default()
                },
            ),
        )
    }

    /// The principal of a signed session cookie; `None` for a bad signature, an
    /// unknown or expired session, or an identity that lost its access.
    fn session_principal(&self, policy: &Policy, signed: &str) -> Option<Principal> {
        let raw = self.keys.verify(super::SESSION_COOKIE, signed)?;
        let digest = crypto::sha256(raw.as_bytes());
        let now = self.now();
        let s = self.sessions.get(&digest, now)?;
        let mut info = PrincipalInfo {
            owner: Some(s.principal.clone()),
            expires: Some(s.expires_at()),
            csrf: Some(self.keys.csrf(raw)),
            session: Some(digest),
            ..Default::default()
        };
        let (kind, name, access) = match s.method {
            Method::Token => {
                let rec = self.tokens.get(s.token_id.as_deref()?)?;
                let access = self.token_access(policy, &rec, now, 0)?;
                info.owner = Some(rec.owner.clone());
                info.token_id = Some(rec.id.clone());
                info.expires = Some(s.expires_at().min(rec.expires_at()));
                (Kind::Token, rec.id, access)
            }
            Method::Password | Method::Oidc => (
                s.principal.kind,
                s.principal.name.clone(),
                Access::of(policy.identity_grants(&s.principal)?),
            ),
        };
        Some(Principal::new(kind, &name, Scheme::Session, access).with_info(info))
    }

    /// Basic user and password: cache, then one argon2 verification (single-flight per
    /// credential for successes; a failure always costs each request a verification).
    async fn password(
        &self,
        policy: &Policy,
        user: &str,
        password: &str,
        admission: Option<&Admission>,
        client: Option<&ClientKey>,
    ) -> Result<Principal, AuthError> {
        let err = |failure| AuthError {
            scheme: "basic",
            failure,
        };
        let mut msg = Zeroizing::new(Vec::with_capacity(user.len() + password.len() + 1));
        msg.extend_from_slice(user.as_bytes());
        msg.push(0);
        msg.extend_from_slice(password.as_bytes());
        let key = crypto::hmac_sha256(&self.hmac_key, &msg);
        let cell = {
            let mut c = self.creds.lock();
            if let Some((p, at)) = c.cache.get(&key)
                && at.elapsed() < CACHE_TTL
            {
                return Ok(p.clone());
            }
            c.inflight.entry(key).or_default().clone()
        };
        // a verification is charged to the address as a failure before it starts
        if admission.is_some_and(|a| !a.reserve()) {
            self.creds.lock().inflight.remove(&key);
            return Err(err(Failure::Limited));
        }
        let (phc, principal) = match policy.users.get(user) {
            Some(u) => (
                u.password.clone(),
                Some(
                    Principal::new(
                        Kind::User,
                        user,
                        Scheme::Basic,
                        Access::of(u.grants.clone()),
                    )
                    .with_info(PrincipalInfo {
                        owner: Some(Identity {
                            kind: Kind::User,
                            name: user.to_string(),
                            groups: Vec::new(),
                            display_name: None,
                        }),
                        ..Default::default()
                    }),
                ),
            ),
            None => (policy.dummy_hash.clone(), None),
        };
        let pw = Zeroizing::new(password.to_string());
        let ran = std::sync::atomic::AtomicBool::new(false);
        let shared = {
            let (phc, principal, pw) = (phc.clone(), principal.clone(), pw.clone());
            cell.get_or_try_init(|| {
                ran.store(true, Ordering::Relaxed);
                async move { self.verify(pw, phc, principal, client).await }
            })
            .await
            .cloned()
        };
        let result = match shared {
            Ok(Some(p)) => Some(p),
            Ok(None) if ran.load(Ordering::Relaxed) => None,
            // another request's verification failed: a failure costs each request one
            Ok(None) => self.verify(pw, phc, principal, client).await?,
            Err(e) => {
                self.creds.lock().inflight.remove(&key);
                return Err(e);
            }
        };
        let mut c = self.creds.lock();
        if let Some(p) = &result {
            if c.cache.len() >= CACHE_MAX {
                c.cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
                if c.cache.len() >= CACHE_MAX {
                    c.cache.clear();
                }
            }
            c.cache.insert(key, (p.clone(), Instant::now()));
        }
        c.inflight.remove(&key);
        drop(c);
        result.ok_or(err(Failure::Invalid))
    }

    /// Check a password for a UI login (no cache; one verification, charged to the
    /// address as a failure before it starts).
    pub async fn check_password(
        &self,
        user: &str,
        password: &str,
        admission: Option<&Admission>,
        client: Option<&ClientKey>,
    ) -> Result<Option<Principal>, AuthError> {
        if admission.is_some_and(|a| !a.reserve()) {
            return Err(AuthError {
                scheme: "session",
                failure: Failure::Limited,
            });
        }
        let policy = self.policy.load_full();
        let (phc, principal) = match policy.users.get(user) {
            Some(u) => (
                u.password.clone(),
                Some(Principal::new(
                    Kind::User,
                    user,
                    Scheme::Session,
                    Access::of(u.grants.clone()),
                )),
            ),
            None => (policy.dummy_hash.clone(), None),
        };
        self.verify(Zeroizing::new(password.to_string()), phc, principal, client)
            .await
    }

    /// One argon2 run under a permit: `Some(principal)` when the password matches.
    async fn verify(
        &self,
        password: Zeroizing<String>,
        phc: String,
        principal: Option<Principal>,
        client: Option<&ClientKey>,
    ) -> Result<Option<Principal>, AuthError> {
        let busy = AuthError {
            scheme: "basic",
            failure: Failure::Busy,
        };
        let a = &self.argon;
        let _client = match client {
            Some(k) => Some(ClientSlot::enter(&a.clients, k, a.per_client).ok_or(busy)?),
            None => None,
        };
        let _permit = match a.permits.try_acquire() {
            Ok(p) => p,
            Err(_) => {
                let _place = Waiting::enter(&a.waiting, a.size * QUEUE_PER_PERMIT).ok_or(busy)?;
                tokio::time::timeout(PERMIT_WAIT, a.permits.acquire())
                    .await
                    .map_err(|_| busy)?
                    .map_err(|_| busy)?
            }
        };
        self.metrics
            .password_verifications
            .fetch_add(1, Ordering::Relaxed);
        let ok = tokio::task::spawn_blocking(move || verify_password(password.as_bytes(), &phc))
            .await
            .unwrap_or(false);
        Ok(if ok { principal } else { None })
    }

    /// The `sparkles_auth_*` metric families (Prometheus text format).
    pub fn render_metrics(&self, o: &mut String) {
        use std::fmt::Write;
        let family = |o: &mut String, name: &str, kind: &str, help: &str| {
            let _ = writeln!(o, "# HELP {name} {help}");
            let _ = writeln!(o, "# TYPE {name} {kind}");
        };
        let m = &self.metrics;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        family(
            o,
            "sparkles_auth_failures_total",
            "counter",
            "Failed authentications by scheme and reason.",
        );
        for (si, scheme) in FAILURE_SCHEMES.iter().enumerate() {
            for (ri, reason) in Failure::ALL.iter().enumerate() {
                let _ = writeln!(
                    o,
                    "sparkles_auth_failures_total{{scheme=\"{scheme}\",reason=\"{}\"}} {}",
                    reason.as_str(),
                    get(&m.failures[si][ri])
                );
            }
        }
        family(
            o,
            "sparkles_auth_denied_total",
            "counter",
            "Requests refused by the auth layer by kind.",
        );
        for (i, kind) in super::Denied::ALL.iter().enumerate() {
            let _ = writeln!(
                o,
                "sparkles_auth_denied_total{{kind=\"{}\"}} {}",
                kind.as_str(),
                get(&m.denied[i])
            );
        }
        family(
            o,
            "sparkles_auth_logins_total",
            "counter",
            "UI logins by method and result.",
        );
        for (mi, method) in ["oidc", "password", "token"].iter().enumerate() {
            for (ri, result) in ["ok", "denied", "error"].iter().enumerate() {
                let _ = writeln!(
                    o,
                    "sparkles_auth_logins_total{{method=\"{method}\",result=\"{result}\"}} {}",
                    get(&m.logins[mi][ri])
                );
            }
        }
        family(
            o,
            "sparkles_auth_tokens_minted_total",
            "counter",
            "API tokens minted by channel.",
        );
        for (i, via) in MINT_VIA.iter().enumerate() {
            let _ = writeln!(
                o,
                "sparkles_auth_tokens_minted_total{{via=\"{via}\"}} {}",
                get(&m.minted[i])
            );
        }
        let now = self.now();
        let (users, tokens, roles) = self.policy.load().counts;
        for (name, kind, help, v) in [
            (
                "sparkles_auth_tokens_revoked_total",
                "counter",
                "API tokens revoked.",
                get(&m.revoked) as usize,
            ),
            (
                "sparkles_auth_tokens_active",
                "gauge",
                "Unexpired minted API tokens.",
                self.tokens.active(now),
            ),
            (
                "sparkles_auth_sessions_active",
                "gauge",
                "Unexpired UI sessions.",
                self.sessions.active(now),
            ),
            (
                "sparkles_auth_device_grants_pending",
                "gauge",
                "Device-code logins waiting for approval.",
                self.cli.pending(now),
            ),
            (
                "sparkles_auth_password_verifications_total",
                "counter",
                "argon2 password verifications (cache hits excluded).",
                get(&m.password_verifications) as usize,
            ),
            (
                "sparkles_auth_password_verifications_running",
                "gauge",
                "argon2 password verifications running.",
                self.argon_load().0,
            ),
            (
                "sparkles_auth_password_verifications_waiting",
                "gauge",
                "Password verifications waiting for a permit (bounded; more are refused as busy).",
                self.argon_load().1,
            ),
            (
                "sparkles_auth_untrusted_proxy_headers_total",
                "counter",
                "Requests with proxy identity headers from untrusted peers (ignored).",
                get(&m.untrusted_proxy_headers) as usize,
            ),
            (
                "sparkles_auth_policy_users",
                "gauge",
                "Configured users.",
                users,
            ),
            (
                "sparkles_auth_policy_tokens",
                "gauge",
                "Configured static API tokens.",
                tokens,
            ),
            (
                "sparkles_auth_policy_roles",
                "gauge",
                "Configured roles.",
                roles,
            ),
        ] {
            family(o, name, kind, help);
            let _ = writeln!(o, "{name} {v}");
        }
        family(
            o,
            "sparkles_auth_reloads_total",
            "counter",
            "Auth configuration reloads by result.",
        );
        for (i, r) in ["ok", "error"].iter().enumerate() {
            let _ = writeln!(
                o,
                "sparkles_auth_reloads_total{{result=\"{r}\"}} {}",
                get(&m.reloads[i])
            );
        }
    }
}

/// The principal of a minted token.
fn minted(rec: TokenRecord, scheme: Scheme, access: Access) -> Principal {
    let exp = rec.expires_at();
    Principal::new(Kind::Token, &rec.id, scheme, access).with_info(PrincipalInfo {
        owner: Some(rec.owner),
        token_id: Some(rec.id),
        expires: Some(exp),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = new_token();
        assert!(well_formed_token(&t), "{t}");
        assert!(!well_formed_token("spk_short"));
        assert!(!well_formed_token(&format!("xyz_{}", &t[4..])));
        let h = token_hash(&t);
        assert!(config::parse_token_hash(&h).is_some());
    }

    #[test]
    fn password_hashes() {
        let h = hash_password_with("pw", 8, 1, 1).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=8,t=1,p=1$"), "{h}");
        assert!(verify_password(b"pw", &h));
        assert!(!verify_password(b"px", &h));
    }
}
