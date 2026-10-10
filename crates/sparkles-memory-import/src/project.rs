//! The project a memory belongs to (spec C18 §8.10.2): a repository's remote URL
//! normalized to host and path, such as `github.com/acme/shop`, or `local.<name>.<hash>`
//! for a directory without a remote.

use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// A project: its directory, the root of its repository and its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// the directory the person works in, absolute
    pub dir: PathBuf,
    /// the repository's root, or `dir` when it is not in a repository
    pub root: PathBuf,
    /// `github.com/acme/shop` or `local.<name>.<hash>`
    pub key: String,
}

impl Project {
    /// The project of a directory: the remote of its repository when it has one.
    pub fn of(dir: &Path) -> Project {
        let dir = absolute(dir);
        let root = repo_root(&dir);
        let key = root
            .as_ref()
            .and_then(|r| remote_url(r))
            .and_then(|u| normalize_remote(&u))
            .unwrap_or_else(|| local_key(root.as_deref().unwrap_or(&dir)));
        Project {
            root: root.unwrap_or_else(|| dir.clone()),
            dir,
            key,
        }
    }

    /// The key as one graph segment: `/` turned into `.`.
    pub fn segment(&self) -> String {
        segment_of(&self.key)
    }
}

/// A key as a graph segment.
pub fn segment_of(key: &str) -> String {
    key.replace('/', ".")
}

/// `local.<name>.<hash>`: the directory's name and the first 8 hex digits of the SHA-256
/// of its absolute path.
fn local_key(dir: &Path) -> String {
    let name: String = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let h = Sha256::digest(dir.to_string_lossy().as_bytes());
    let hex: String = h.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("local.{name}.{hex}")
}

/// An absolute path with `.` and `..` removed without touching the file system.
pub fn absolute(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    normalize(&p)
}

/// `.` and `..` removed lexically.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// The nearest directory at or above `dir` with a `.git` entry.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(x) = d {
        if x.join(".git").exists() {
            return Some(x.to_path_buf());
        }
        d = x.parent();
    }
    None
}

/// The git directory that holds the configuration: `.git`, or for a worktree the
/// common directory its `gitdir` line leads to.
fn config_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        root.join(gitdir)
    };
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = c.trim();
            Some(normalize(&if Path::new(c).is_absolute() {
                PathBuf::from(c)
            } else {
                gitdir.join(c)
            }))
        }
        Err(_) => Some(gitdir),
    }
}

/// The URL of the `origin` remote, else of the first remote.
fn remote_url(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_dir(root)?.join("config")).ok()?;
    let mut section = String::new();
    let mut first: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.to_string();
            continue;
        }
        if !section.starts_with("[remote ") {
            continue;
        }
        if let Some((k, v)) = t.split_once('=')
            && k.trim() == "url"
        {
            let v = v.trim().to_string();
            if section == "[remote \"origin\"]" {
                return Some(v);
            }
            first.get_or_insert(v);
        }
    }
    first
}

/// A remote URL as `host/path`: `git@github.com:acme/shop.git` and
/// `https://user@GitHub.com/acme/shop` both give `github.com/acme/shop`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let u = url.trim();
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (h, p) = rest.split_once('/')?;
        // a port is not part of the key
        (h.split(':').next().unwrap_or(h), p)
    } else if let Some((h, p)) = u.split_once(':') {
        // scp-like: [user@]host:path
        (h.rsplit_once('@').map_or(h, |(_, h)| h), p)
    } else {
        return None;
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes() {
        for u in [
            "git@github.com:acme/shop.git",
            "https://github.com/acme/shop.git",
            "https://user:pw@GitHub.com/acme/shop/",
            "ssh://git@github.com:22/acme/shop.git",
        ] {
            assert_eq!(
                normalize_remote(u).as_deref(),
                Some("github.com/acme/shop"),
                "{u}"
            );
        }
        assert_eq!(normalize_remote("/srv/repo.git"), None);
    }

    #[test]
    fn repository_and_local_keys() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("shop");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@example.org:x/y\n[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n",
        )
        .unwrap();
        let p = Project::of(&repo.join("src"));
        assert_eq!(p.key, "github.com/acme/shop");
        assert_eq!(p.root, absolute(&repo));
        assert_eq!(p.segment(), "github.com.acme.shop");
        // a worktree
        let wt = d.path().join("wt");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert_eq!(Project::of(&wt).key, "github.com/acme/shop");
        // no repository
        let plain = d.path().join("notes dir");
        std::fs::create_dir_all(&plain).unwrap();
        let k = Project::of(&plain).key;
        assert!(k.starts_with("local.notes-dir."), "{k}");
        assert_eq!(k.len(), "local.notes-dir.".len() + 8);
    }
}
