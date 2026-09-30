//! The source of a backup: a capture of a live store ([`Source::from`] a
//! `sparkles::store::BackupCapture`), or of a closed database directory (the CLI with
//! a stopped database, and tests).

use crate::BackupError;
use crate::error::Result;
use sparkles::commit::CommitInfo;
use sparkles::store::{BackupCapture, CapturedFile, LeaseGuard, Store, StoreOptions};
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
    /// directory)
    pub lease: LeaseGuard,
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
    use sparkles::sparql::QueryOptions;
    use sparkles::sparql::update::update;

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
        assert_eq!(src.index_format, sparkles::builder::FORMAT_VERSION);
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
        assert!(e.to_string().contains("in use"), "{e}");
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
        assert_eq!(e.code(), Code::InvalidRequest);
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
