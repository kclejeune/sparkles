//! `$XDG_CONFIG_HOME/sparkles/credentials.toml` (default `~/.config/…`): the tokens of
//! `sparkles auth login`, per server. Directory 0700, file 0600, written atomically.
//!
//! The file's format and path come from the Rust client (`sparkles_client::credentials`),
//! so the CLI and programs using the client read the same file the same way.

use anyhow::{Context, Result};
pub use sparkles_client::credentials::{Credentials, ServerCreds};

/// Read the file (empty when absent); warns when others can read it.
pub fn load() -> Result<Credentials> {
    let p = sparkles_client::credentials::path()?;
    if sparkles_client::credentials::readable_by_others(&p) {
        eprintln!(
            "warning: {} is readable by others; run: chmod 600 {}",
            p.display(),
            p.display()
        );
    }
    Ok(Credentials::load_from(&p)?)
}

/// Write the file atomically, with the directory at 0700 and the file at 0600.
pub fn save(c: &Credentials) -> Result<()> {
    let p = sparkles_client::credentials::path()?;
    let dir = p.parent().context("credentials path")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    crate::auth::write_private(&p, toml::to_string_pretty(c)?.as_bytes())
}
