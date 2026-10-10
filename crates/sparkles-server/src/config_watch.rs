//! `serve --watch-config`: reload the configuration files when they change.
//!
//! Kubernetes updates a mounted ConfigMap or Secret by writing a new directory of files
//! and swapping the `..data` symlink to it, and it sends the process no signal. Editors
//! and configuration management tools also replace files by renaming. The watcher
//! therefore polls: every [`INTERVAL`] it reads each watched file through its path,
//! following symlinks, and hashes the contents. Comparing contents rather than
//! modification times catches a symlink swap and ignores a touch that changes nothing.
//!
//! A change is acted on once the files have stayed the same for one more poll and at
//! least [`SETTLE`], so that several files written one after another, or a file written
//! in pieces, give one reload. The reload is the one SIGHUP runs: the watcher raises
//! SIGHUP in its own process, which every reloadable part (the settings file, the model
//! configuration, auth, backup and rate limits, and the TLS files) already listens for.
//! Each part logs whether its file loaded, and a file that does not load leaves that
//! part's previous configuration in place, exactly as with `kill -HUP`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often the watched files are read.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How long the files must stay unchanged after a change before the reload runs.
pub const SETTLE: Duration = Duration::from_secs(1);

/// The timing of a watcher; [`Timing::default`] is what `serve` uses.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub interval: Duration,
    pub settle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            interval: INTERVAL,
            settle: SETTLE,
        }
    }
}

/// A running watcher; it stops when dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Watcher {
    /// Keep the watcher running for the life of the process.
    pub fn detach(self) {
        std::mem::forget(self);
    }
}

/// The contents of `path` as a hash, or `None` when it cannot be read (missing, a
/// dangling symlink, no permission).
fn fingerprint(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

fn fingerprints(paths: &[PathBuf]) -> Vec<Option<u64>> {
    paths.iter().map(|p| fingerprint(p)).collect()
}

/// Watch `paths` on a thread of its own and call `on_change` with the files that
/// changed, once they have settled (see the module documentation).
pub fn spawn(
    paths: Vec<PathBuf>,
    timing: Timing,
    on_change: impl Fn(&[PathBuf]) + Send + 'static,
) -> std::io::Result<Watcher> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    // the state to compare with is the one when this returns
    let mut applied = fingerprints(&paths);
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            let mut seen = applied.clone();
            let mut since = Instant::now();
            loop {
                std::thread::sleep(timing.interval);
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let now = fingerprints(&paths);
                if now != seen {
                    seen = now;
                    since = Instant::now();
                    continue;
                }
                if seen != applied && since.elapsed() >= timing.settle {
                    let changed: Vec<PathBuf> = paths
                        .iter()
                        .zip(seen.iter().zip(&applied))
                        .filter(|(_, (a, b))| a != b)
                        .map(|(p, _)| p.clone())
                        .collect();
                    applied = seen.clone();
                    on_change(&changed);
                }
            }
        })?;
    Ok(Watcher { stop })
}

/// `serve --watch-config`: watch the configuration files the server was started with
/// and raise SIGHUP when they change. Call it after every SIGHUP listener is
/// registered, since SIGHUP without a listener ends the process.
#[cfg(unix)]
pub fn start(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let mut paths = paths;
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        tracing::warn!(
            "--watch-config has nothing to watch: none of --settings, --model-config, \
             --auth-config, --backup-config or --rate-limit-config is given"
        );
        return Ok(());
    }
    for p in &paths {
        tracing::info!("watching {} for changes", p.display());
    }
    let w = spawn(paths, Timing::default(), |changed| {
        let names: Vec<String> = changed.iter().map(|p| p.display().to_string()).collect();
        tracing::info!(
            "configuration changed ({}): reloading as on SIGHUP",
            names.join(", ")
        );
        // SAFETY: kill(2) with this process's own id and a signal that has a handler
        if unsafe { libc::kill(libc::getpid(), libc::SIGHUP) } != 0 {
            tracing::error!(
                "cannot raise SIGHUP for the reload: {}",
                std::io::Error::last_os_error()
            );
        }
    })?;
    w.detach();
    Ok(())
}

/// Without Unix there is no SIGHUP reload to run.
#[cfg(not(unix))]
pub fn start(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let _ = paths;
    anyhow::bail!("--watch-config needs a Unix platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const FAST: Timing = Timing {
        interval: Duration::from_millis(20),
        settle: Duration::from_millis(150),
    };

    type Calls = Arc<Mutex<Vec<Vec<PathBuf>>>>;

    fn watch(paths: Vec<PathBuf>) -> (Watcher, Calls) {
        let calls: Calls = Arc::default();
        let c = calls.clone();
        let w = spawn(paths, FAST, move |changed| {
            c.lock().unwrap().push(changed.to_vec())
        })
        .unwrap();
        (w, calls)
    }

    /// Wait until `calls` has `n` entries (or fail after 5 s), then a while longer
    /// to see that no more arrive.
    fn expect_calls(calls: &Calls, n: usize) -> Vec<Vec<PathBuf>> {
        let t0 = Instant::now();
        while calls.lock().unwrap().len() < n {
            assert!(t0.elapsed() < Duration::from_secs(5), "no reload");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(FAST.settle * 4);
        let c = calls.lock().unwrap().clone();
        assert_eq!(c.len(), n, "{c:?}");
        c
    }

    /// The layout kubelet writes for a ConfigMap volume: the visible file is a
    /// symlink to `..data/<name>`, and `..data` a symlink to a timestamped directory
    /// that an update replaces by renaming a new symlink over `..data`.
    #[cfg(unix)]
    struct ConfigMapDir {
        root: PathBuf,
        version: usize,
    }

    #[cfg(unix)]
    impl ConfigMapDir {
        fn new(root: &Path, files: &[(&str, &str)]) -> ConfigMapDir {
            let mut d = ConfigMapDir {
                root: root.to_path_buf(),
                version: 0,
            };
            d.update(files);
            for (name, _) in files {
                std::os::unix::fs::symlink(Path::new("..data").join(name), root.join(name))
                    .unwrap();
            }
            d
        }

        fn update(&mut self, files: &[(&str, &str)]) {
            self.version += 1;
            let dir = format!("..2026_10_10_00_00_0{}.000", self.version);
            std::fs::create_dir(self.root.join(&dir)).unwrap();
            for (name, body) in files {
                std::fs::write(self.root.join(&dir).join(name), body).unwrap();
            }
            let tmp = self.root.join("..data_tmp");
            std::os::unix::fs::symlink(&dir, &tmp).unwrap();
            std::fs::rename(&tmp, self.root.join("..data")).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn configmap_symlink_swap_reloads_once() {
        let d = tempfile::tempdir().unwrap();
        let mut cm = ConfigMapDir::new(
            d.path(),
            &[("settings.json", "{}"), ("models.json", "{\"models\":{}}")],
        );
        let settings = d.path().join("settings.json");
        let (_w, calls) = watch(vec![settings.clone(), d.path().join("models.json")]);
        std::thread::sleep(FAST.interval * 3);
        assert!(
            calls.lock().unwrap().is_empty(),
            "a reload without a change"
        );
        cm.update(&[
            ("settings.json", "{\"defaults\":{}}"),
            ("models.json", "{\"models\":{}}"),
        ]);
        let c = expect_calls(&calls, 1);
        // only the file whose contents changed is named
        assert_eq!(c[0], vec![settings]);
    }

    #[test]
    fn writes_in_a_burst_give_one_reload() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.json");
        let b = d.path().join("b.json");
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "1").unwrap();
        let (_w, calls) = watch(vec![a.clone(), b.clone()]);
        // changes closer together than the settle time, across a poll or two
        for i in 2..8 {
            std::fs::write(&a, i.to_string()).unwrap();
            std::fs::write(&b, i.to_string()).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        let c = expect_calls(&calls, 1);
        assert_eq!(c[0], vec![a.clone(), b]);
        // a later change is a second reload
        std::fs::write(&a, "later").unwrap();
        expect_calls(&calls, 2);
    }

    #[test]
    fn same_contents_rewritten_is_no_change() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.json");
        std::fs::write(&a, "same").unwrap();
        let (_w, calls) = watch(vec![a.clone()]);
        std::fs::write(&a, "same").unwrap();
        // replaced by a rename, as editors do
        let tmp = d.path().join("a.json.tmp");
        std::fs::write(&tmp, "same").unwrap();
        std::fs::rename(&tmp, &a).unwrap();
        std::thread::sleep(FAST.settle * 4);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_file_that_appears_or_goes_is_a_change() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.json");
        let (_w, calls) = watch(vec![a.clone()]);
        std::fs::write(&a, "{}").unwrap();
        expect_calls(&calls, 1);
        std::fs::remove_file(&a).unwrap();
        expect_calls(&calls, 2);
    }

    #[test]
    fn dropping_the_watcher_stops_it() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.json");
        std::fs::write(&a, "1").unwrap();
        let (w, calls) = watch(vec![a.clone()]);
        drop(w);
        std::thread::sleep(FAST.interval * 2);
        std::fs::write(&a, "2").unwrap();
        std::thread::sleep(FAST.settle * 4);
        assert!(calls.lock().unwrap().is_empty());
    }
}
