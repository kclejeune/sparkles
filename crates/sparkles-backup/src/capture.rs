//! The source of a backup: a capture of a live store ([`Source::from`] a
//! `sparkles_core::store::BackupCapture`), or of a closed database directory (the CLI with
//! a stopped database, and tests).

use crate::BackupError;
use crate::error::Result;
use sparkles_core::commit::CommitInfo;
use sparkles_core::store::{BackupCapture, CapturedFile, LeaseGuard, Store, StoreOptions};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

/// What a backup uploads: the files of one database at one commit.
#[derive(Debug)]
pub struct Source {
    pub dataset_id: Uuid,
    /// the captured commit
    pub commit: CommitInfo,
    /// the generation directory (`gen-NNNN`)
    pub generation: String,
    /// the index format of the generation
    pub index_format: u32,
    /// every file, with its kind and captured length (see `BackupCapture::files`)
    pub files: Vec<CapturedFile>,
    /// writer-lock hold time of the capture (zero for a closed directory)
    pub lock_hold: Duration,
    /// keeps the generation until the source is dropped (holds nothing for a closed
    /// directory; for an in-memory dataset it removes the temporary database)
    pub lease: LeaseGuard,
    /// the source is an in-memory dataset (the manifest's `dataset.type` is `mem`)
    pub in_memory: bool,
    pub branch: Option<sparkles_core::store::BackupBranch>,
    pub next_ordinal: u32,
    /// the dataset's branches other than `main`, which the backup leaves out
    pub branches_omitted: u64,
}

impl From<BackupCapture> for Source {
    fn from(c: BackupCapture) -> Source {
        Source {
            dataset_id: c.dataset_id,
            commit: c.commit,
            generation: c.generation,
            index_format: c.index_format,
            files: c.files,
            lock_hold: c.lock_hold,
            lease: c.lease,
            in_memory: c.in_memory,
            branch: c.branch,
            next_ordinal: c.next_ordinal,
            branches_omitted: c.branches_omitted,
        }
    }
}

impl Source {
    /// A source for the database directory `dir` that no other process has open (the
    /// store's "in use by another process" error otherwise): the same files and the
    /// same head commit a capture of that database, opened, would give. For the CLI
    /// (`backup create --loc` on a stopped database) and tests.
    ///
    /// The database is opened (which takes its process lock and recovers it as any
    /// open does: a torn WAL tail is cut, the commit catalog caught up) and captured;
    /// it stays open, and locked, until the source is dropped. A directory without
    /// `CURRENT` is not a database: `invalid-request`.
    pub fn from_closed_dir(dir: &Path) -> Result<Source> {
        if !dir.join("CURRENT").is_file() {
            return Err(BackupError::new(
                crate::Code::InvalidRequest,
                format!("{} is not a database directory", dir.display()),
            ));
        }
        let opts = StoreOptions {
            cache_bytes: 16 << 20,
            result_cache_bytes: 0,
            history_cache_bytes: 16 << 20,
            // a stopped database may require a write guard; nothing is written here
            unvalidated_writes: true,
            ..StoreOptions::default()
        };
        let store = Store::open(dir, opts)?;
        Ok(Source::from(store.into_backup_capture("offline")?))
    }

    /// Capture a selected branch of a stopped dataset, keeping its owner locked
    /// until the upload finishes. Linked branches are materialized independently.
    pub fn from_closed_branch(
        dir: &Path,
        branch: &str,
        options: &sparkles_core::store::MemoryCaptureOptions,
    ) -> Result<Source> {
        if branch == "main" {
            return Self::from_closed_dir(dir);
        }
        if !dir.join("CURRENT").is_file() {
            return Err(BackupError::new(
                crate::Code::InvalidRequest,
                "not a database directory",
            ));
        }
        let store = Store::open(
            dir,
            StoreOptions {
                unvalidated_writes: true,
                ..Default::default()
            },
        )?;
        let selected = store.branch(branch)?;
        let mut cap = if selected.snapshot().generation.linked().is_some() {
            selected.materialized_backup_capture("offline", options)?
        } else {
            selected.backup_capture_with(
                "offline",
                &sparkles_core::guard::WriteOptions {
                    cancel: options.cancel.clone(),
                    deadline: options.deadline,
                    no_wait: options.no_wait,
                    ..Default::default()
                },
            )?
        };
        drop(selected);
        cap.lease.hold_owner(Box::new(move || drop(store)));
        Ok(cap.into())
    }

    /// The file at `path` (`gen-0001/wal.log`, `CURRENT`, …).
    pub fn file(&self, path: &str) -> Option<&CapturedFile> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Read a whole file (up to its captured length) into memory.
    pub fn read_file(f: &CapturedFile) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; f.len as usize];
        f.read_exact_at(0, &mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Code;
    use sparkles_core::sparql::QueryOptions;
    use sparkles_core::sparql::update::update;

    fn db(dir: &Path) -> (Uuid, u64) {
        let s = Store::open(dir, StoreOptions::default()).unwrap();
        for i in 0..3 {
            let u = format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}");
            update(&s, &u, &QueryOptions::default()).unwrap();
        }
        (s.dataset_id(), s.head_commit().seq)
    }

    #[test]
    fn a_closed_directory_is_captured_and_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("db");
        let (id, head) = db(&dir);
        let src = Source::from_closed_dir(&dir).unwrap();
        assert_eq!(src.dataset_id, id);
        assert_eq!(src.commit.seq, head);
        assert_eq!(src.generation, "gen-0001");
        assert_eq!(src.index_format, sparkles_core::builder::FORMAT_VERSION);
        assert_eq!(src.lease.generation(), 1);
        let current = src.file("CURRENT").unwrap();
        assert_eq!(Source::read_file(current).unwrap(), b"gen-0001");
        let wal = src.file("gen-0001/wal.log").unwrap();
        assert_eq!(
            wal.len,
            std::fs::metadata(dir.join("gen-0001/wal.log"))
                .unwrap()
                .len()
        );
        assert!(src.file("commits.bin").is_some());
        for f in &src.files {
            assert!(crate::layout::valid_backup_path(&f.path), "{}", f.path);
        }
        // the directory stays locked while the source lives
        let e = Store::open(&dir, StoreOptions::default()).err().unwrap();
        assert!(matches!(e, sparkles_core::Error::Locked { .. }), "{e}");
        drop(src);
        Store::open(&dir, StoreOptions::default()).unwrap();
    }

    #[test]
    fn only_database_directories_are_captured() {
        let tmp = tempfile::tempdir().unwrap();
        let e = Source::from_closed_dir(tmp.path()).unwrap_err();
        assert_eq!(e.code(), Code::InvalidRequest);
        let missing = tmp.path().join("missing");
        let e = Source::from_closed_dir(&missing).unwrap_err();
        assert_eq!(e.code(), Code::InvalidRequest);
        assert!(!missing.exists());
        // an open database is not closed
        let dir = tmp.path().join("db");
        db(&dir);
        let _open = Store::open(&dir, StoreOptions::default()).unwrap();
        let e = Source::from_closed_dir(&dir).unwrap_err();
        assert_eq!(e.code(), Code::DatasetBusy);
    }

    #[test]
    fn a_live_capture_converts() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("db");
        db(&dir);
        let s = Store::open(&dir, StoreOptions::default()).unwrap();
        let src = Source::from(s.backup_capture("b1").unwrap());
        assert_eq!(src.lease.label(), "b1");
        assert_eq!(src.commit, s.head_commit());
        let e = BackupError::from(
            Store::in_memory(Default::default())
                .backup_capture("b")
                .unwrap_err(),
        );
        assert_eq!(e.code(), Code::BackupUnsupported);
    }

    /// A backup of an in-memory dataset through an `fs` repository: verified, restored
    /// as a persistent database with the same content, head and id, and backed up
    /// again unchanged without new pieces.
    #[test]
    fn an_in_memory_dataset_round_trips() {
        use crate::{
            CreateOptions, OpenEnv, RepoConfig, Repository, RestoreOptions, VerifyLevel,
            VerifyOptions,
        };
        use sparkles_core::store::MemoryCaptureOptions;
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::in_memory(StoreOptions::default());
        for i in 0..50 {
            let u = format!(
                "INSERT DATA {{ GRAPH <urn:g{}> {{ <urn:s{i}> <urn:p> {i} }} }}",
                i % 3
            );
            update(&s, &u, &QueryOptions::default()).unwrap();
        }
        s.set_prefix("ex", "http://example.org/").unwrap();
        let mut want = Vec::new();
        s.dump_nquads(&mut want).unwrap();
        let head = s.head_commit();
        let scratch = tmp.path().join("scratch");
        let capture = |label: &str| {
            let o = MemoryCaptureOptions {
                tmp_dir: scratch.clone(),
                ..Default::default()
            };
            Source::from(s.memory_backup_capture(label, &o).unwrap())
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let repo_dir = tmp.path().join("repo");
            let url = format!("file://{}", repo_dir.display());
            let cfg = RepoConfig::from_url("t", &url).unwrap();
            let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
            let o = |name: &str| CreateOptions {
                name: name.into(),
                dataset_name: "mem".into(),
                ..Default::default()
            };
            let b1 = repo.create(capture("b1"), &o("b1")).await.unwrap();
            assert_eq!(b1.commit.seq, head.seq);
            assert_eq!(b1.commit.quads, 50);
            assert_eq!(b1.dataset.id, s.dataset_id());
            let m = repo.manifest("b1").await.unwrap();
            assert_eq!(m.dataset.kind, "mem");
            // the temporary database is gone
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
            // unchanged: the rebuilt generation has the same pieces
            let b2 = repo.create(capture("b2"), &o("b2")).await.unwrap();
            let m2 = repo.manifest("b2").await.unwrap();
            let gen_bytes: u64 = m2
                .files
                .iter()
                .filter(|f| f.path.starts_with("gen-") && !f.path.ends_with("commit.json"))
                .map(|f| f.size)
                .sum();
            assert!(gen_bytes > 0);
            assert!(
                b2.added_bytes < gen_bytes / 2,
                "{} added of {gen_bytes}",
                b2.added_bytes
            );
            let vo = VerifyOptions {
                level: VerifyLevel::Restore,
                tmp_dir: Some(tmp.path().to_path_buf()),
                ..Default::default()
            };
            let v = repo.verify(&["b1".to_string()], &vo).await.unwrap();
            assert_eq!(v.status, crate::VerifyStatus::Ok, "{v:?}");
            // a new persistent dataset; the live in-memory dataset has the id
            let out = tmp.path().join("restored");
            let ro = RestoreOptions {
                id_in_use: std::sync::Arc::new(|_| false),
                ..Default::default()
            };
            let r = repo.restore("b1", &out, &ro).await.unwrap();
            assert_eq!(r.identity, "kept");
            let restored = Store::open(&out, StoreOptions::default()).unwrap();
            assert_eq!(restored.dataset_id(), s.dataset_id());
            assert_eq!(restored.head_commit().seq, head.seq);
            let mut got = Vec::new();
            restored.dump_nquads(&mut got).unwrap();
            let lines = |b: &[u8]| {
                let mut v: Vec<&str> = std::str::from_utf8(b).unwrap().lines().collect();
                v.sort_unstable();
                v.join("\n")
            };
            assert_eq!(lines(&got), lines(&want));
            assert_eq!(
                restored.prefixes().get("ex").map(String::as_str),
                Some("http://example.org/")
            );
        });
    }

    /// A compaction while a backup uploads: the upload reads the leased generation, the
    /// backup restores to the captured commit, and the generation is collected after.
    #[test]
    fn a_backup_survives_a_compaction_during_its_upload() {
        use crate::{CreateOptions, OpenEnv, RepoConfig, Repository, RestoreOptions};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("db");
        db(&dir);
        let s = Store::open(&dir, StoreOptions::default()).unwrap();
        let mut want = Vec::new();
        s.dump_nquads(&mut want).unwrap();
        let head = s.head_commit();
        let src = Source::from(s.backup_capture("b1").unwrap());
        let u = "INSERT DATA { <urn:late> <urn:p> 1 }";
        update(&s, u, &QueryOptions::default()).unwrap();
        s.compact().unwrap();
        let held: Vec<String> = s.history().generations[0]
            .held_by
            .iter()
            .map(|h| h.to_string())
            .collect();
        assert_eq!(held, ["backup:b1"]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cfg = RepoConfig::from_url("t", "memory://").unwrap();
            let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
            let o = CreateOptions {
                name: "b1".into(),
                dataset_name: "ds".into(),
                ..Default::default()
            };
            let b = repo.create(src, &o).await.unwrap();
            assert_eq!(b.commit.seq, head.seq);
            // the lease is gone with the source
            s.try_collect_history();
            assert_eq!(s.history().bytes, 0);
            let out = tmp.path().join("restored");
            let o = RestoreOptions {
                id_in_use: std::sync::Arc::new(|_| true),
                ..Default::default()
            };
            let r = repo.restore("b1", &out, &o).await.unwrap();
            assert_eq!(r.identity, "new");
            let restored = Store::open(&out, StoreOptions::default()).unwrap();
            assert_eq!(restored.head_commit().seq, head.seq);
            let mut got = Vec::new();
            restored.dump_nquads(&mut got).unwrap();
            assert_eq!(got, want);
        });
    }
}
