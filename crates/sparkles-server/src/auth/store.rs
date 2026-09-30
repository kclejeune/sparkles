//! Private files under `<data>/auth/` (directory 0700, files 0600) and time helpers.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

/// Unix seconds.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn parse_rfc3339(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

/// Create `dir` (and parents) with mode 0700.
pub fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", dir.display()))?;
    }
    Ok(())
}

/// Replace `path` durably with mode 0600: a temporary file created 0600 before any byte
/// is written, synced, renamed over `path`, then the directory synced.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    crate::state::sync_dir(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_files() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("auth");
        private_dir(&dir).unwrap();
        let f = dir.join("x.json");
        write_private(&f, b"{}").unwrap();
        write_private(&f, b"[]").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"[]");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&f), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        assert_eq!(parse_rfc3339(&rfc3339(1_800_000_000)), Some(1_800_000_000));
    }
}
