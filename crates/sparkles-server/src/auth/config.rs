//! The auth configuration file (TOML): parsing, validation and warnings.

use super::{Level, ServerPerm};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// `[anonymous]`, `[roles.NAME]`: grants without roles.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsCfg {
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserCfg {
    pub name: String,
    /// argon2id PHC string; absent for users who sign in only through OIDC or a proxy
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
}

impl std::fmt::Debug for UserCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserCfg")
            .field("name", &self.name)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("roles", &self.roles)
            .field("datasets", &self.datasets)
            .field("server", &self.server)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCfg {
    pub name: String,
    /// `sha256:<64 lowercase hex>` of the whole token string
    pub hash: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
    /// RFC 3339
    #[serde(default)]
    pub expires: Option<String>,
}

impl std::fmt::Debug for TokenCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenCfg")
            .field("name", &self.name)
            .field("hash", &"<redacted>")
            .field("roles", &self.roles)
            .field("datasets", &self.datasets)
            .field("server", &self.server)
            .field("expires", &self.expires)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorsCfg {
    #[serde(default)]
    pub origins: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub version: u32,
    #[serde(default = "default_realm")]
    pub realm: String,
    #[serde(default)]
    pub anonymous: GrantsCfg,
    #[serde(default)]
    pub roles: BTreeMap<String, GrantsCfg>,
    #[serde(default)]
    pub users: Vec<UserCfg>,
    #[serde(default)]
    pub tokens: Vec<TokenCfg>,
    #[serde(default)]
    pub cors: CorsCfg,
}

fn default_realm() -> String {
    "sparkles".into()
}

/// OWASP's minimum argon2id parameters (m in KiB).
pub const OWASP_M: u32 = 19456;
pub const OWASP_T: u32 = 2;
pub const OWASP_P: u32 = 1;

/// User, token and role names: `[A-Za-z0-9_.@-]{1,64}`.
pub fn valid_principal_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// A dataset pattern: a dataset name that may contain `*`.
pub fn valid_pattern(p: &str) -> bool {
    p == "*" || crate::state::valid_name(&p.replace('*', "x")) && !p.starts_with('.')
}

/// Parse an RFC 3339 timestamp into Unix seconds.
pub fn parse_rfc3339(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("invalid RFC 3339 timestamp '{s}'"))?
        .timestamp())
}

/// `sha256:<64 lowercase hex>` → the digest.
pub fn parse_token_hash(h: &str) -> Option<[u8; 32]> {
    let hex = h.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// argon2 parameters `(m, t, p)` of a PHC string, if it is argon2id.
pub fn argon2id_params(phc: &str) -> Option<(u32, u32, u32)> {
    let rest = phc.strip_prefix("$argon2id$")?;
    let mut parts = rest.split('$');
    let first = parts.next()?;
    let params = if first.starts_with("v=") {
        parts.next()?
    } else {
        first
    };
    let (mut m, mut t, mut p) = (None, None, None);
    for kv in params.split(',') {
        let (k, v) = kv.split_once('=')?;
        let v: u32 = v.parse().ok()?;
        match k {
            "m" => m = Some(v),
            "t" => t = Some(v),
            "p" => p = Some(v),
            _ => {}
        }
    }
    Some((m?, t?, p?))
}

fn check_grants(
    what: &str,
    datasets: &BTreeMap<String, Level>,
    roles: &[String],
    known_roles: &BTreeMap<String, GrantsCfg>,
) -> Result<()> {
    for pat in datasets.keys() {
        if !valid_pattern(pat) {
            bail!("{what}: invalid dataset pattern '{pat}'");
        }
    }
    for r in roles {
        if !known_roles.contains_key(r) {
            bail!("{what}: unknown role '{r}'");
        }
    }
    Ok(())
}

/// A valid CORS origin: `scheme://host[:port]`, no path, not `*`.
fn valid_origin(o: &str) -> bool {
    let Some((scheme, rest)) = o.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && !rest.is_empty()
        && !rest.contains(['/', '?', '#', '*', '@', ' '])
}

impl FileConfig {
    /// Parse (errors carry the line and column) and validate.
    pub fn parse(text: &str) -> Result<FileConfig> {
        let cfg: FileConfig = toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Read and parse `path`; the returned warnings include the file mode check.
    pub fn load(path: &Path) -> Result<(FileConfig, Vec<String>)> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the auth configuration {}", path.display()))?;
        let cfg = FileConfig::parse(&text)
            .with_context(|| format!("invalid auth configuration {}", path.display()))?;
        let mut warnings = cfg.warnings();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(path)
                && m.permissions().mode() & 0o077 != 0
            {
                warnings.push(format!(
                    "{} is readable by group or others (mode {:o}); use 0600 or 0640",
                    path.display(),
                    m.permissions().mode() & 0o777
                ));
            }
        }
        Ok((cfg, warnings))
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("version must be 1");
        }
        if self.realm.is_empty() || self.realm.contains(['"', '\\']) || !self.realm.is_ascii() {
            bail!("realm must be printable ASCII without quotes or backslashes");
        }
        for name in self.roles.keys() {
            if !valid_principal_name(name) {
                bail!("invalid role name '{name}'");
            }
        }
        check_grants("[anonymous]", &self.anonymous.datasets, &[], &self.roles)?;
        for (name, r) in &self.roles {
            check_grants(&format!("role {name}"), &r.datasets, &[], &self.roles)?;
        }
        let mut names = BTreeSet::new();
        for u in &self.users {
            if !valid_principal_name(&u.name) {
                bail!("invalid user name '{}'", u.name);
            }
            if !names.insert(u.name.as_str()) {
                bail!("duplicate user '{}'", u.name);
            }
            if let Some(pw) = &u.password
                && argon2id_params(pw).is_none()
            {
                bail!(
                    "user {}: password must be an argon2id PHC string ($argon2id$…, see `sparkles auth hash`)",
                    u.name
                );
            }
            check_grants(
                &format!("user {}", u.name),
                &u.datasets,
                &u.roles,
                &self.roles,
            )?;
        }
        let mut names = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        for t in &self.tokens {
            if !valid_principal_name(&t.name) {
                bail!("invalid token name '{}'", t.name);
            }
            if !names.insert(t.name.as_str()) {
                bail!("duplicate token '{}'", t.name);
            }
            if parse_token_hash(&t.hash).is_none() {
                bail!(
                    "token {}: hash must be sha256: followed by 64 lowercase hex characters",
                    t.name
                );
            }
            if !hashes.insert(t.hash.as_str()) {
                bail!("token {}: duplicate hash", t.name);
            }
            if let Some(e) = &t.expires {
                parse_rfc3339(e).with_context(|| format!("token {}: expires", t.name))?;
            }
            check_grants(
                &format!("token {}", t.name),
                &t.datasets,
                &t.roles,
                &self.roles,
            )?;
        }
        for o in &self.cors.origins {
            if !valid_origin(o) {
                bail!("cors.origins: '{o}' is not scheme://host[:port]");
            }
        }
        Ok(())
    }

    /// Problems worth a WARN that do not stop the server.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        for u in &self.users {
            if let Some((m, t, p)) = u.password.as_deref().and_then(argon2id_params)
                && (m < OWASP_M || t < OWASP_T || p < OWASP_P)
            {
                w.push(format!(
                    "user {}: argon2 parameters m={m},t={t},p={p} are below the OWASP minimum m={OWASP_M},t={OWASP_T},p={OWASP_P}",
                    u.name
                ));
            }
            if u.datasets.is_empty() && u.server.is_empty() && u.roles.is_empty() {
                w.push(format!("user {} has no grants", u.name));
            }
        }
        for t in &self.tokens {
            if t.datasets.is_empty() && t.server.is_empty() && t.roles.is_empty() {
                w.push(format!("token {} has no grants", t.name));
            }
        }
        if self.anonymous.datasets.values().any(|l| *l > Level::Read)
            || !self.anonymous.server.is_empty()
        {
            w.push("anonymous holds write, admin or a server permission".into());
        }
        w
    }

    /// `3 users, 2 tokens, 1 role, anonymous: public=read`
    pub fn summary(&self) -> String {
        let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
        let anon: Vec<String> = self
            .anonymous
            .datasets
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str()))
            .chain(self.anonymous.server.iter().map(|s| s.as_str().to_string()))
            .collect();
        format!(
            "{}, {}, {}, anonymous: {}",
            plural(self.users.len(), "user"),
            plural(self.tokens.len(), "token"),
            plural(self.roles.len(), "role"),
            if anon.is_empty() {
                "none".to_string()
            } else {
                anon.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha256:6b3a55e0261b0304143f805a24924d0c1c44524821305f31d9277843b8a10f4e";

    #[test]
    fn parses_the_example() {
        let cfg = FileConfig::parse(&format!(
            r#"
version = 1
[anonymous]
datasets = {{ public = "read" }}
[roles.wiki-editors]
datasets = {{ wiki = "write", "wiki-*" = "write" }}
[[users]]
name = "alice"
password = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaA"
server = ["server-admin"]
[[users]]
name = "bob"
password = "$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaA"
roles = ["wiki-editors"]
[[tokens]]
name = "etl"
hash = "{HASH}"
datasets = {{ wiki = "write" }}
expires = "2027-06-30T00:00:00Z"
[cors]
origins = ["https://yasgui.example.org"]
"#
        ))
        .unwrap();
        assert_eq!(cfg.users.len(), 2);
        assert_eq!(cfg.realm, "sparkles");
        let w = cfg.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("bob"));
        assert_eq!(
            cfg.summary(),
            "2 users, 1 token, 1 role, anonymous: public=read"
        );
    }

    #[test]
    fn errors_name_the_key_and_position() {
        let e = FileConfig::parse("version = 1\n[[users]]\nname = \"x\"\ndataset = {}\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("dataset"), "{e}");
        assert!(e.contains("line 4"), "{e}");
        let bad = |t: &str| FileConfig::parse(t).unwrap_err().to_string();
        assert!(bad("version = 2").contains("version"));
        assert!(bad("version = 1\n[[users]]\nname = \"a:b\"").contains("user name"));
        assert!(
            bad("version = 1\n[[users]]\nname = \"a\"\npassword = \"plain\"").contains("argon2id")
        );
        assert!(
            bad("version = 1\n[[users]]\nname = \"a\"\nroles = [\"nope\"]")
                .contains("unknown role")
        );
        assert!(
            bad("version = 1\n[[tokens]]\nname = \"a\"\nhash = \"sha256:AB\"").contains("hash")
        );
        assert!(
            bad("version = 1\n[anonymous]\ndatasets = { \"a/b\" = \"read\" }").contains("pattern")
        );
        assert!(bad("version = 1\n[anonymous]\ndatasets = { a = \"owner\" }").contains("owner"));
        assert!(bad("version = 1\n[anonymous]\nserver = [\"root\"]").contains("root"));
        assert!(bad("version = 1\n[cors]\norigins = [\"*\"]").contains("cors"));
        assert!(
            bad("version = 1\n[cors]\norigins = [\"https://a.example/path\"]").contains("cors")
        );
        assert!(
            bad(&format!(
                "version = 1\n[[tokens]]\nname = \"a\"\nhash = \"{HASH}\"\nexpires = \"tomorrow\""
            ))
            .contains("expires")
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(
            argon2id_params("$argon2id$v=19$m=19456,t=2,p=1$abc$def"),
            Some((19456, 2, 1))
        );
        assert_eq!(
            argon2id_params("$argon2i$v=19$m=19456,t=2,p=1$abc$def"),
            None
        );
        assert!(parse_token_hash(HASH).is_some());
        assert!(parse_token_hash(&HASH.to_uppercase()).is_none());
        assert!(valid_pattern("*"));
        assert!(valid_pattern("team-*"));
        assert!(valid_pattern("a*b*"));
        assert!(!valid_pattern("a b"));
        assert!(!valid_pattern(""));
        assert!(valid_origin("http://localhost:5173"));
        assert!(!valid_origin("localhost:5173"));
    }
}
