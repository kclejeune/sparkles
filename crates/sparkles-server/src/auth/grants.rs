//! CLI logins: RFC 8628 device grants and loopback codes (RFC 8252 §7.3 with PKCE).
//! Both live in memory; they last minutes.

use super::crypto;
use parking_lot::Mutex;
use std::collections::HashMap;
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
    pub token: Zeroizing<String>,
    pub token_id: String,
    /// log name of the approver
    pub principal: String,
    pub expires_in: i64,
}

#[derive(Clone)]
enum Status {
    Pending,
    Approved(Issued),
    Denied,
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
                match s {
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
            expires_in: 60,
        }
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
