//! The loaded policy and authentication of requests.

use super::config::{self, FileConfig};
use super::{Grants, Kind, Level, Principal, Scheme, ServerPerm, crypto};
use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use axum::http::{HeaderMap, header};
use base64::Engine;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Entries of the verified-credential cache, and how long they stay valid.
const CACHE_MAX: usize = 10_000;
const CACHE_TTL: Duration = Duration::from_secs(300);
/// How long a password verification may wait for a permit before `busy`.
const PERMIT_WAIT: Duration = Duration::from_secs(5);

/// A configured token: expiry (Unix seconds) and principal.
struct TokenEntry {
    expires: Option<i64>,
    principal: Principal,
}

struct UserEntry {
    /// argon2id PHC string; `None` for users without a password (OIDC / proxy only)
    password: Option<String>,
    principal: Principal,
}

/// One validated configuration, swapped as a whole on reload.
pub struct Policy {
    pub realm: String,
    anonymous: Principal,
    users: HashMap<String, UserEntry>,
    tokens: HashMap<[u8; 32], TokenEntry>,
    pub cors_origins: Vec<String>,
    /// verified in place of the hash of an unknown user, so timing reveals no names
    dummy_hash: String,
    pub counts: (usize, usize, usize),
}

fn grants_of(
    datasets: &std::collections::BTreeMap<String, Level>,
    server: &[ServerPerm],
    roles: &[String],
    role_grants: &HashMap<String, Grants>,
) -> Grants {
    let mut g = Grants {
        datasets: datasets.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        server: Vec::new(),
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

impl Policy {
    pub fn build(cfg: &FileConfig) -> Result<Policy> {
        let roles: HashMap<String, Grants> = cfg
            .roles
            .iter()
            .map(|(n, r)| {
                (
                    n.clone(),
                    grants_of(&r.datasets, &r.server, &[], &HashMap::new()),
                )
            })
            .collect();
        let anonymous = Principal::new(
            Kind::Anonymous,
            "anonymous",
            Scheme::None,
            Arc::new(grants_of(
                &cfg.anonymous.datasets,
                &cfg.anonymous.server,
                &[],
                &roles,
            )),
        );
        let mut users = HashMap::new();
        for u in &cfg.users {
            let g = grants_of(&u.datasets, &u.server, &u.roles, &roles);
            users.insert(
                u.name.clone(),
                UserEntry {
                    password: u.password.clone(),
                    principal: Principal::new(Kind::User, &u.name, Scheme::Basic, Arc::new(g)),
                },
            );
        }
        let mut tokens = HashMap::new();
        for t in &cfg.tokens {
            let g = grants_of(&t.datasets, &t.server, &t.roles, &roles);
            let digest = config::parse_token_hash(&t.hash).context("token hash")?;
            tokens.insert(
                digest,
                TokenEntry {
                    expires: t
                        .expires
                        .as_deref()
                        .map(config::parse_rfc3339)
                        .transpose()?,
                    principal: Principal::new(Kind::Token, &t.name, Scheme::Bearer, Arc::new(g)),
                },
            );
        }
        // the dummy uses the parameters of a configured password, so an unknown name
        // costs as much as a known one
        let (m, t, p) = cfg
            .users
            .iter()
            .find_map(|u| u.password.as_deref().and_then(config::argon2id_params))
            .unwrap_or((config::OWASP_M, config::OWASP_T, config::OWASP_P));
        let dummy_hash = hash_password_with(&crypto::random_token(24), m, t, p)?;
        Ok(Policy {
            realm: cfg.realm.clone(),
            anonymous,
            users,
            tokens,
            cors_origins: cfg.cors.origins.clone(),
            dummy_hash,
            counts: (cfg.users.len(), cfg.tokens.len(), cfg.roles.len()),
        })
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

/// A well-formed static token: `spk_` + 43 base64url characters.
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
}

impl Failure {
    pub const ALL: [Failure; 4] = [
        Failure::Malformed,
        Failure::Invalid,
        Failure::Expired,
        Failure::Busy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Failure::Malformed => "malformed",
            Failure::Invalid => "invalid",
            Failure::Expired => "expired",
            Failure::Busy => "busy",
        }
    }
}

/// A failed authentication: the scheme tried and why it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthError {
    pub scheme: Scheme,
    pub failure: Failure,
}

/// Counters of the auth layer (closed label sets; no principal label).
#[derive(Default)]
pub struct AuthMetrics {
    /// `[scheme][reason]`, scheme: basic, bearer
    pub failures: [[AtomicU64; 4]; 2],
    /// unauthenticated, forbidden, hidden, cross_origin
    pub denied: [AtomicU64; 4],
    pub password_verifications: AtomicU64,
    /// ok, error
    pub reloads: [AtomicU64; 2],
}

/// A password verification in progress, shared by concurrent requests.
type Inflight = Arc<tokio::sync::OnceCell<Option<Principal>>>;

/// Verified Basic credentials and verifications in progress, keyed by
/// `HMAC(k, user ‖ 0 ‖ password)`; one lock, so a request never misses both.
#[derive(Default)]
struct Creds {
    cache: HashMap<[u8; 32], (Principal, Instant)>,
    inflight: HashMap<[u8; 32], Inflight>,
}

/// The auth state of a server: the current policy and the password machinery.
pub struct Auth {
    path: Option<PathBuf>,
    policy: ArcSwap<Policy>,
    creds: Mutex<Creds>,
    argon: tokio::sync::Semaphore,
    hmac_key: Vec<u8>,
    pub metrics: AuthMetrics,
}

impl Auth {
    /// Load and validate `path`; returns the warnings to log.
    pub fn load(path: &Path) -> Result<(Auth, Vec<String>)> {
        let (cfg, warnings) = FileConfig::load(path)?;
        let mut auth = Auth::from_config(&cfg)?;
        auth.path = Some(path.to_path_buf());
        Ok((auth, warnings))
    }

    pub fn from_config(cfg: &FileConfig) -> Result<Auth> {
        let permits = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .max(1);
        Ok(Auth {
            path: None,
            policy: ArcSwap::from_pointee(Policy::build(cfg)?),
            creds: Mutex::new(Creds::default()),
            argon: tokio::sync::Semaphore::new(permits),
            hmac_key: crypto::random_bytes(32),
            metrics: AuthMetrics::default(),
        })
    }

    pub fn policy(&self) -> Arc<Policy> {
        self.policy.load_full()
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
            self.policy.store(Arc::new(p));
            self.creds.lock().cache.clear();
            Ok(warnings)
        })();
        self.metrics.reloads[usize::from(r.is_err())].fetch_add(1, Ordering::Relaxed);
        r
    }

    /// The `sparkles_auth_*` metric families (Prometheus text format).
    pub fn render_metrics(&self, o: &mut String) {
        use std::fmt::Write;
        let family = |o: &mut String, name: &str, kind: &str, help: &str| {
            let _ = writeln!(o, "# HELP {name} {help}");
            let _ = writeln!(o, "# TYPE {name} {kind}");
        };
        let m = &self.metrics;
        family(
            o,
            "sparkles_auth_failures_total",
            "counter",
            "Failed authentications by scheme and reason.",
        );
        for (si, scheme) in ["basic", "bearer"].iter().enumerate() {
            for (ri, reason) in Failure::ALL.iter().enumerate() {
                let _ = writeln!(
                    o,
                    "sparkles_auth_failures_total{{scheme=\"{scheme}\",reason=\"{}\"}} {}",
                    reason.as_str(),
                    m.failures[si][ri].load(Ordering::Relaxed)
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
                m.denied[i].load(Ordering::Relaxed)
            );
        }
        family(
            o,
            "sparkles_auth_password_verifications_total",
            "counter",
            "argon2 password verifications (cache hits excluded).",
        );
        let _ = writeln!(
            o,
            "sparkles_auth_password_verifications_total {}",
            m.password_verifications.load(Ordering::Relaxed)
        );
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
                m.reloads[i].load(Ordering::Relaxed)
            );
        }
        let (users, tokens, roles) = self.policy.load().counts;
        for (name, help, v) in [
            ("sparkles_auth_policy_users", "Configured users.", users),
            (
                "sparkles_auth_policy_tokens",
                "Configured API tokens.",
                tokens,
            ),
            ("sparkles_auth_policy_roles", "Configured roles.", roles),
        ] {
            family(o, name, "gauge", help);
            let _ = writeln!(o, "{name} {v}");
        }
    }

    pub fn anonymous(&self) -> Principal {
        self.policy.load().anonymous.clone()
    }

    pub fn count_failure(&self, e: AuthError) {
        let s = match e.scheme {
            Scheme::Basic => 0,
            _ => 1,
        };
        let r = Failure::ALL
            .iter()
            .position(|f| *f == e.failure)
            .unwrap_or(0);
        self.metrics.failures[s][r].fetch_add(1, Ordering::Relaxed);
    }

    /// Authenticate from the `Authorization` header; none is anonymous. Invalid
    /// credentials are an error, never anonymous.
    pub async fn authenticate(&self, h: &HeaderMap) -> Result<Principal, AuthError> {
        let Some(v) = h.get(header::AUTHORIZATION) else {
            return Ok(self.anonymous());
        };
        let malformed = |scheme| AuthError {
            scheme,
            failure: Failure::Malformed,
        };
        let v = v.to_str().map_err(|_| malformed(Scheme::Bearer))?;
        let (scheme, rest) = v.trim().split_once(' ').unwrap_or((v.trim(), ""));
        let rest = rest.trim();
        if scheme.eq_ignore_ascii_case("bearer") {
            if rest.is_empty() {
                return Err(malformed(Scheme::Bearer));
            }
            return self.token(rest, Scheme::Bearer);
        }
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(malformed(Scheme::Bearer));
        }
        let decoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(rest)
                .map_err(|_| malformed(Scheme::Basic))?,
        );
        let text = std::str::from_utf8(&decoded).map_err(|_| malformed(Scheme::Basic))?;
        let (user, password) = text.split_once(':').ok_or(malformed(Scheme::Basic))?;
        if password.starts_with("spk_") {
            // Basic-only clients carry a token as the password; the user is ignored
            return self.token(password, Scheme::Basic);
        }
        self.password(user, password).await
    }

    fn token(&self, token: &str, scheme: Scheme) -> Result<Principal, AuthError> {
        let err = |failure| AuthError { scheme, failure };
        if !well_formed_token(token) {
            return Err(err(Failure::Invalid));
        }
        let digest = crypto::sha256(token.as_bytes());
        let policy = self.policy.load();
        let e = policy.tokens.get(&digest).ok_or(err(Failure::Invalid))?;
        if e.expires
            .is_some_and(|x| x <= chrono::Utc::now().timestamp())
        {
            return Err(err(Failure::Expired));
        }
        let mut p = e.principal.clone();
        p.scheme = scheme;
        Ok(p)
    }

    /// Basic user and password: cache, then one argon2 verification (single-flight per
    /// credential for successes; a failure always costs each request a verification).
    async fn password(&self, user: &str, password: &str) -> Result<Principal, AuthError> {
        let err = |failure| AuthError {
            scheme: Scheme::Basic,
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
        let policy = self.policy.load_full();
        let (phc, principal) = match policy.users.get(user) {
            Some(UserEntry {
                password: Some(h),
                principal,
            }) => (h.clone(), Some(principal.clone())),
            _ => (policy.dummy_hash.clone(), None),
        };
        let pw = Zeroizing::new(password.to_string());
        let ran = std::sync::atomic::AtomicBool::new(false);
        let shared = {
            let (phc, principal, pw) = (phc.clone(), principal.clone(), pw.clone());
            cell.get_or_try_init(|| {
                ran.store(true, Ordering::Relaxed);
                async move { self.verify(pw, phc, principal).await }
            })
            .await
            .cloned()
        };
        let result = match shared {
            Ok(Some(p)) => Some(p),
            Ok(None) if ran.load(Ordering::Relaxed) => None,
            // another request's verification failed: a failure costs each request one
            Ok(None) => self.verify(pw, phc, principal).await?,
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

    /// One argon2 run under a permit: `Some(principal)` when the password matches.
    async fn verify(
        &self,
        password: Zeroizing<String>,
        phc: String,
        principal: Option<Principal>,
    ) -> Result<Option<Principal>, AuthError> {
        let busy = AuthError {
            scheme: Scheme::Basic,
            failure: Failure::Busy,
        };
        let _permit = tokio::time::timeout(PERMIT_WAIT, self.argon.acquire())
            .await
            .map_err(|_| busy)?
            .map_err(|_| busy)?;
        self.metrics
            .password_verifications
            .fetch_add(1, Ordering::Relaxed);
        let ok = tokio::task::spawn_blocking(move || verify_password(password.as_bytes(), &phc))
            .await
            .unwrap_or(false);
        Ok(if ok { principal } else { None })
    }
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
