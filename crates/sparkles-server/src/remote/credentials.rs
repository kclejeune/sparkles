//! `$XDG_CONFIG_HOME/sparkles/credentials.toml` (default `~/.config/…`): the tokens of
//! `sparkles auth login`, per server. Directory 0700, file 0600, written atomically.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerCreds>,
}

/// The credentials file path.
pub fn path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .context("neither XDG_CONFIG_HOME nor HOME is set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("sparkles").join("credentials.toml"))
}

impl Credentials {
    /// Read the file (empty when absent); warns when others can read it.
    pub fn load() -> Result<Credentials> {
        let p = path()?;
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Credentials::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(&p)
                && m.permissions().mode() & 0o077 != 0
            {
                eprintln!(
                    "warning: {} is readable by others; run: chmod 600 {}",
                    p.display(),
                    p.display()
                );
            }
        }
        toml::from_str(&text).with_context(|| format!("{} is not valid", p.display()))
    }

    pub fn save(&self) -> Result<()> {
        let p = path()?;
        let dir = p.parent().context("credentials path")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        crate::auth::write_private(&p, toml::to_string_pretty(self)?.as_bytes())
    }
}
