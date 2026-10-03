//! The credentials file of `sparkles auth login`: `$XDG_CONFIG_HOME/sparkles/credentials.toml`,
//! by default `~/.config/sparkles/credentials.toml`, with one API token per server. The
//! servers are keyed by their URL as [`normalize`] writes it, so the CLI and this crate
//! must normalize the same way; the CLI uses this module for both.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The login of one server.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerCreds {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// The whole file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerCreds>,
}

/// The credentials file's path.
pub fn path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| Error::config("neither XDG_CONFIG_HOME nor HOME is set"))?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("sparkles").join("credentials.toml"))
}

impl Credentials {
    /// Read the file at [`path`]; an absent file is empty.
    pub fn load() -> Result<Credentials> {
        Credentials::load_from(&path()?)
    }

    /// Read a credentials file; an absent file is empty.
    pub fn load_from(p: &Path) -> Result<Credentials> {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Credentials::default());
            }
            Err(e) => {
                return Err(Error::Io {
                    path: p.display().to_string(),
                    source: e,
                });
            }
        };
        toml::from_str(&text)
            .map_err(|e| Error::config(format!("{} is not valid: {e}", p.display())))
    }

    /// The token for a server: `SPARKLES_TOKEN` when set and not empty, else the saved
    /// one for the server's normalized URL.
    pub fn token_for(&self, normalized_base: &str) -> Option<String> {
        std::env::var("SPARKLES_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .or_else(|| self.servers.get(normalized_base).map(|c| c.token.clone()))
    }
}

/// Whether users other than the owner may read the file (Unix permissions).
pub fn readable_by_others(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(p) {
            return m.permissions().mode() & 0o077 != 0;
        }
    }
    let _ = p;
    false
}

/// Whether a host is the local machine.
pub(crate) fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// `scheme://host[:port][/path]`, with a lower-case host and no trailing slash; `https://`
/// is assumed without a scheme. Plain `http` to a host other than loopback is refused
/// unless `insecure`, because tokens are bearer secrets.
pub fn normalize(url: &str, insecure: bool) -> Result<String> {
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let u = reqwest::Url::parse(&with_scheme)
        .map_err(|e| Error::config(format!("invalid URL '{url}': {e}")))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(Error::config(format!(
            "invalid server URL '{url}' (expected http(s)://host[:port])"
        )));
    }
    let host = u.host_str().unwrap_or_default();
    if u.scheme() == "http" && !is_loopback(host) && !insecure {
        return Err(Error::config(format!(
            "refusing plain http to {host}: tokens would travel in clear text (use https, or allow insecure http)"
        )));
    }
    let mut s = format!("{}://{host}", u.scheme());
    if let Some(p) = u.port() {
        s.push_str(&format!(":{p}"));
    }
    s.push_str(u.path().trim_end_matches('/'));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        assert_eq!(
            normalize("HTTPS://Sparql.Example.org/", false).unwrap(),
            "https://sparql.example.org"
        );
        assert_eq!(
            normalize("sparql.example.org:8443/base/", false).unwrap(),
            "https://sparql.example.org:8443/base"
        );
        assert_eq!(
            normalize("http://127.0.0.1:3030", false).unwrap(),
            "http://127.0.0.1:3030"
        );
        assert!(normalize("http://example.org", false).is_err());
        assert_eq!(
            normalize("http://example.org", true).unwrap(),
            "http://example.org"
        );
        assert!(normalize("ftp://example.org", true).is_err());
    }

    #[test]
    fn reads_the_cli_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("credentials.toml");
        assert!(Credentials::load_from(&p).unwrap().servers.is_empty());
        std::fs::write(
            &p,
            "default_server = \"https://a.example\"\n\n[servers.\"https://a.example\"]\ntoken = \"spk_x\"\ntoken_id = \"t1\"\n",
        )
        .unwrap();
        let c = Credentials::load_from(&p).unwrap();
        assert_eq!(c.default_server.as_deref(), Some("https://a.example"));
        assert_eq!(c.servers["https://a.example"].token, "spk_x");
        std::fs::write(&p, "servers = 3").unwrap();
        assert!(Credentials::load_from(&p).is_err());
    }
}
