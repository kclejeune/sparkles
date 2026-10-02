//! CLI logins: RFC 8628 device grants and loopback codes (RFC 8252 §7.3 with PKCE).
//!
//! Device grants survive a restart: `<data>/auth/device-grants.json` (0600) keeps them
//! with the digest of the device code, never the code or a token. A grant approved
//! before a restart and not yet retrieved hands the CLI a new secret for the token that
//! was minted at approval ([`Poll::Reissue`]): the token's record is kept, and only the
//! secret, which was in memory, changes. Loopback codes live in memory; they last two
//! minutes.

use super::crypto;
use super::store::{self, parse_rfc3339, rfc3339};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Device grants expire after this many seconds; clients poll this often.
pub const DEVICE_TTL: i64 = 600;
pub const DEVICE_INTERVAL: i64 = 5;
/// Pending device grants kept at most.
pub const MAX_DEVICE_GRANTS: usize = 1_000;
/// Loopback codes are valid this long.
pub const LOOPBACK_TTL: i64 = 120;

/// Unambiguous user-code alphabet (no 0/O, 1/I).
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// A token handed to a CLI once its login is approved.
#[derive(Clone)]
pub struct Issued {
    /// empty for a grant approved before a restart (the secret was in memory only)
    pub token: Zeroizing<String>,
    pub token_id: String,
    /// log name of the approver
    pub principal: String,
    /// Unix seconds at which the token expires
    pub expires_at: i64,
}

#[derive(Clone)]
enum Status {
    Pending,
    Approved(Issued),
    Denied,
}

/// A device grant as `device-grants.json` keeps it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredGrant {
    /// `sha256:<hex>` of the device code
    device: String,
    user_code: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    hostname: String,
    expires: String,
    interval: i64,
    /// `pending`, `approved` or `denied`
    status: String,
    /// an approved grant: the token minted for it, its approver and expiry
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_expires: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct GrantFile {
    format: u32,
    grants: Vec<StoredGrant>,
}

#[derive(Clone)]
struct DeviceGrant {
    user_code: String,
    label: String,
    hostname: String,
    expires: i64,
    interval: i64,
    last_poll: Option<i64>,
    status: Status,
}

/// What the approval page shows about a pending device login.
pub struct DeviceInfo {
    pub user_code: String,
    pub label: String,
    pub hostname: String,
    pub expires_in: i64,
    pub status: &'static str,
}

/// The answer to a device poll.
pub enum Poll {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Token(Issued),
    /// approved before a restart: give the CLI a new secret for this token
    Reissue(Issued),
}

struct Loopback {
    challenge: String,
    port: u16,
    issued: Issued,
    expires: i64,
}

#[derive(Default)]
struct Inner {
    /// by `sha256(device_code)`
    devices: HashMap<[u8; 32], DeviceGrant>,
    /// normalized user code → device digest
    user_codes: HashMap<String, [u8; 32]>,
    /// by `sha256(code)`
    loopback: HashMap<[u8; 32], Loopback>,
}

#[derive(Default)]
pub struct CliGrants {
    inner: Mutex<Inner>,
    /// where device grants are kept (`None`: in memory only)
    path: Option<PathBuf>,
}

/// `abcd-efgh`, `ABCDEFGH` → `ABCDEFGH` when it is a well-formed code.
pub fn normalize_user_code(c: &str) -> Option<String> {
    let s: String = c
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == 8 && s.bytes().all(|b| ALPHABET.contains(&b))).then_some(s)
}

fn format_user_code(n: &str) -> String {
    format!("{}-{}", &n[..4], &n[4..])
}

fn prune(inner: &mut Inner, now: i64) {
    let gone: Vec<[u8; 32]> = inner
        .devices
        .iter()
        .filter(|(_, g)| g.expires <= now)
        .map(|(k, _)| *k)
        .collect();
    for k in gone {
        if let Some(g) = inner.devices.remove(&k) {
            inner.user_codes.remove(&g.user_code.replace('-', ""));
        }
    }
    inner.loopback.retain(|_, l| l.expires > now);
}

impl CliGrants {
    /// Open (or start) the store of device grants at `path`, dropping expired grants; a
    /// corrupt file is an error, never silently reset.
    pub fn open(path: PathBuf, now: i64) -> Result<CliGrants> {
        let mut inner = Inner::default();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let f: GrantFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is corrupt", path.display()))?;
                if f.format != 1 {
                    bail!("{}: unknown format {}", path.display(), f.format);
                }
                for g in f.grants {
                    let bad = || format!("{}: a bad grant", path.display());
                    let digest = super::config::parse_token_hash(&g.device).with_context(bad)?;
                    let code = normalize_user_code(&g.user_code).with_context(bad)?;
                    let expires = parse_rfc3339(&g.expires).with_context(bad)?;
                    if expires <= now {
                        continue;
                    }
                    let status = match g.status.as_str() {
                        "pending" => Status::Pending,
                        "denied" => Status::Denied,
                        "approved" => Status::Approved(Issued {
                            token: Zeroizing::new(String::new()),
                            token_id: g.token_id.with_context(bad)?,
                            principal: g.principal.unwrap_or_default(),
                            expires_at: g
                                .token_expires
                                .as_deref()
                                .and_then(parse_rfc3339)
                                .with_context(bad)?,
                        }),
                        _ => bail!("{}", bad()),
                    };
                    inner.user_codes.insert(code.clone(), digest);
                    inner.devices.insert(
                        digest,
                        DeviceGrant {
                            user_code: format_user_code(&code),
                            label: g.label,
                            hostname: g.hostname,
                            expires,
                            interval: g.interval.clamp(DEVICE_INTERVAL, 60),
                            last_poll: None,
                            status,
                        },
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
        Ok(CliGrants {
            inner: Mutex::new(inner),
            path: Some(path),
        })
    }

    /// Write the device grants (no device code and no token is written).
    fn save(&self, inner: &Inner) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut grants: Vec<StoredGrant> = inner
            .devices
            .iter()
            .map(|(d, g)| {
                let (status, issued) = match &g.status {
                    Status::Pending => ("pending", None),
                    Status::Denied => ("denied", None),
                    Status::Approved(i) => ("approved", Some(i)),
                };
                StoredGrant {
                    device: format!("sha256:{}", crypto::hex(d)),
                    user_code: g.user_code.clone(),
                    label: g.label.clone(),
                    hostname: g.hostname.clone(),
                    expires: rfc3339(g.expires),
                    interval: g.interval,
                    status: status.into(),
                    token_id: issued.map(|i| i.token_id.clone()),
                    principal: issued.map(|i| i.principal.clone()),
                    token_expires: issued.map(|i| rfc3339(i.expires_at)),
                }
            })
            .collect();
        grants.sort_by(|a, b| a.expires.cmp(&b.expires));
        store::write_private(
            path,
            &serde_json::to_vec_pretty(&GrantFile { format: 1, grants })?,
        )
    }

    /// [`save`](Self::save), logging a failure: the grant still works until a restart.
    fn persist(&self, inner: &Inner) {
        if let Err(e) = self.save(inner) {
            tracing::error!("cannot write the device grants: {e:#}");
        }
    }

    /// Start a device grant; `None` when too many are pending.
    pub fn start(&self, label: &str, hostname: &str, now: i64) -> Option<(String, String)> {
        let mut inner = self.inner.lock();
        prune(&mut inner, now);
        if inner.devices.len() >= MAX_DEVICE_GRANTS {
            return None;
        }
        let device_code = crypto::hex(&crypto::random_bytes(32));
        let user_code = loop {
            let code: String = crypto::random_bytes(8)
                .iter()
                .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
                .collect();
            if !inner.user_codes.contains_key(&code) {
                break code;
            }
        };
        let digest = crypto::sha256(device_code.as_bytes());
        inner.user_codes.insert(user_code.clone(), digest);
        inner.devices.insert(
            digest,
            DeviceGrant {
                user_code: format_user_code(&user_code),
                label: label.to_string(),
                hostname: hostname.to_string(),
                expires: now + DEVICE_TTL,
                interval: DEVICE_INTERVAL,
                last_poll: None,
                status: Status::Pending,
            },
        );
        self.persist(&inner);
        Some((device_code, format_user_code(&user_code)))
    }

    /// A pending (or decided, unexpired) grant by user code. The caller counts failed
    /// lookups (the auth throttle's `device-code` limit).
    pub fn lookup(&self, user_code: &str, now: i64) -> Option<DeviceInfo> {
        let mut inner = self.inner.lock();
        prune(&mut inner, now);
        let found = normalize_user_code(user_code)
            .and_then(|c| inner.user_codes.get(&c).copied())
            .and_then(|d| inner.devices.get(&d).cloned());
        found.map(|g| DeviceInfo {
            user_code: g.user_code,
            label: g.label,
            hostname: g.hostname,
            expires_in: g.expires - now,
            status: match g.status {
                Status::Pending => "pending",
                Status::Approved(_) => "approved",
                Status::Denied => "denied",
            },
        })
    }

    /// Approve (with the minted token) or deny (`None`) a pending grant. `false` when
    /// there is no pending grant with that code.
    pub fn decide(&self, user_code: &str, issued: Option<Issued>, now: i64) -> bool {
        let mut inner = self.inner.lock();
        prune(&mut inner, now);
        let Some(d) =
            normalize_user_code(user_code).and_then(|c| inner.user_codes.get(&c).copied())
        else {
            return false;
        };
        match inner.devices.get_mut(&d) {
            Some(g) if matches!(g.status, Status::Pending) => {
                g.status = match issued {
                    Some(i) => Status::Approved(i),
                    None => Status::Denied,
                };
                self.persist(&inner);
                true
            }
            _ => false,
        }
    }

    /// Whether a grant with this user code is pending (for the approval checks).
    pub fn is_pending(&self, user_code: &str, now: i64) -> bool {
        let mut inner = self.inner.lock();
        prune(&mut inner, now);
        normalize_user_code(user_code)
            .and_then(|c| inner.user_codes.get(&c).copied())
            .and_then(|d| inner.devices.get(&d))
            .is_some_and(|g| matches!(g.status, Status::Pending))
    }

    /// The label and hostname of a grant (for the token record).
    pub fn client_of(&self, user_code: &str) -> Option<(String, String)> {
        let inner = self.inner.lock();
        let d = normalize_user_code(user_code).and_then(|c| inner.user_codes.get(&c).copied())?;
        inner
            .devices
            .get(&d)
            .map(|g| (g.label.clone(), g.hostname.clone()))
    }

    /// A poll of the token endpoint (RFC 8628 §3.4, §3.5).
    pub fn poll(&self, device_code: &str, now: i64) -> Poll {
        let digest = crypto::sha256(device_code.as_bytes());
        let mut inner = self.inner.lock();
        let Some(g) = inner.devices.get_mut(&digest) else {
            return Poll::Expired;
        };
        if g.expires <= now {
            let code = g.user_code.replace('-', "");
            inner.devices.remove(&digest);
            inner.user_codes.remove(&code);
            self.persist(&inner);
            return Poll::Expired;
        }
        let too_fast = g.last_poll.is_some_and(|t| now - t < g.interval);
        g.last_poll = Some(now);
        match g.status.clone() {
            Status::Pending if too_fast => {
                g.interval += 5;
                Poll::SlowDown
            }
            Status::Pending => Poll::Pending,
            // one-time retrieval
            s => {
                let code = g.user_code.replace('-', "");
                inner.devices.remove(&digest);
                inner.user_codes.remove(&code);
                self.persist(&inner);
                match s {
                    Status::Approved(i) if i.token.is_empty() => Poll::Reissue(i),
                    Status::Approved(i) => Poll::Token(i),
                    _ => Poll::Denied,
                }
            }
        }
    }

    /// Keep a minted token under a one-time code bound to `(challenge, port)`.
    pub fn loopback_insert(&self, challenge: &str, port: u16, issued: Issued, now: i64) -> String {
        let code = crypto::random_token(32);
        let mut inner = self.inner.lock();
        prune(&mut inner, now);
        inner.loopback.insert(
            crypto::sha256(code.as_bytes()),
            Loopback {
                challenge: challenge.to_string(),
                port,
                issued,
                expires: now + LOOPBACK_TTL,
            },
        );
        code
    }

    /// Redeem a loopback code; any attempt consumes it.
    pub fn loopback_redeem(
        &self,
        code: &str,
        verifier: &str,
        port: Option<u16>,
        now: i64,
    ) -> Option<Issued> {
        let l = self
            .inner
            .lock()
            .loopback
            .remove(&crypto::sha256(code.as_bytes()))?;
        let ok = l.expires > now
            && port.is_none_or(|p| p == l.port)
            && crypto::ct_eq(
                crypto::pkce_challenge(verifier).as_bytes(),
                l.challenge.as_bytes(),
            );
        ok.then_some(l.issued)
    }

    /// Device grants waiting for a decision.
    pub fn pending(&self, now: i64) -> usize {
        self.inner
            .lock()
            .devices
            .values()
            .filter(|g| g.expires > now && matches!(g.status, Status::Pending))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issued() -> Issued {
        Issued {
            token: Zeroizing::new("spk_x".into()),
            token_id: "tok_x".into(),
            principal: "oidc:a".into(),
            expires_at: 4000,
        }
    }

    #[test]
    fn device_grants_survive_a_restart() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("device-grants.json");
        let g = CliGrants::open(path.clone(), 1000).unwrap();
        let (pending, _) = g.start("cli", "h1", 1000).unwrap();
        let (approved, uc) = g.start("cli", "h2", 1000).unwrap();
        let (denied, uc2) = g.start("cli", "h3", 1000).unwrap();
        assert!(g.decide(&uc, Some(issued()), 1001));
        assert!(g.decide(&uc2, None, 1001));
        let text = std::fs::read_to_string(&path).unwrap();
        // neither device codes nor tokens are written
        for secret in [&pending, &approved, &denied, &"spk_x".to_string()] {
            assert!(!text.contains(secret.as_str()), "{text}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let g = CliGrants::open(path.clone(), 1100).unwrap();
        assert_eq!(g.pending(1100), 1);
        assert!(matches!(g.poll(&pending, 1100), Poll::Pending));
        assert!(g.lookup(&uc, 1100).is_some());
        match g.poll(&approved, 1100) {
            Poll::Reissue(i) => {
                assert!(i.token.is_empty());
                assert_eq!(i.token_id, "tok_x");
                assert_eq!(i.expires_at, 4000);
            }
            _ => panic!("an approved grant was not kept"),
        }
        assert!(matches!(g.poll(&approved, 1110), Poll::Expired));
        assert!(matches!(g.poll(&denied, 1100), Poll::Denied));
        // expired grants are dropped when the store is opened
        let g = CliGrants::open(path.clone(), 1601).unwrap();
        assert!(matches!(g.poll(&pending, 1601), Poll::Expired));
        std::fs::write(&path, "{").unwrap();
        assert!(CliGrants::open(path, 0).is_err());
    }

    #[test]
    fn device_lifecycle() {
        let g = CliGrants::default();
        let (dc, uc) = g.start("cli", "h", 1000).unwrap();
        assert!(
            regex::Regex::new("^[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}$")
                .unwrap()
                .is_match(&uc)
        );
        assert!(matches!(g.poll(&dc, 1000), Poll::Pending));
        assert!(matches!(g.poll(&dc, 1002), Poll::SlowDown));
        assert!(g.lookup(&uc.to_lowercase(), 1003).is_some());
        assert!(g.lookup("AAAA-AAAA", 1003).is_none());
        assert!(g.decide(&uc, Some(issued()), 1003));
        assert!(!g.decide(&uc, None, 1003));
        assert!(matches!(g.poll(&dc, 1020), Poll::Token(_)));
        assert!(matches!(g.poll(&dc, 1030), Poll::Expired));
        let (dc, uc) = g.start("cli", "h", 2000).unwrap();
        assert!(g.decide(&uc, None, 2000));
        assert!(matches!(g.poll(&dc, 2000), Poll::Denied));
        let (dc, _) = g.start("cli", "h", 3000).unwrap();
        assert!(matches!(g.poll(&dc, 3601), Poll::Expired));
    }

    #[test]
    fn loopback_codes_are_one_time() {
        let g = CliGrants::default();
        let verifier = "v".repeat(43);
        let c = crypto::pkce_challenge(&verifier);
        let code = g.loopback_insert(&c, 50123, issued(), 0);
        assert!(g.loopback_redeem(&code, "wrong", Some(50123), 1).is_none());
        assert!(
            g.loopback_redeem(&code, &verifier, Some(50123), 1)
                .is_none()
        );
        let code = g.loopback_insert(&c, 50123, issued(), 0);
        assert!(
            g.loopback_redeem(&code, &verifier, Some(50124), 1)
                .is_none()
        );
        let code = g.loopback_insert(&c, 50123, issued(), 0);
        assert!(
            g.loopback_redeem(&code, &verifier, Some(50123), 121)
                .is_none()
        );
        let code = g.loopback_insert(&c, 50123, issued(), 0);
        assert!(
            g.loopback_redeem(&code, &verifier, Some(50123), 1)
                .is_some()
        );
        assert!(
            g.loopback_redeem(&code, &verifier, Some(50123), 1)
                .is_none()
        );
    }
}
