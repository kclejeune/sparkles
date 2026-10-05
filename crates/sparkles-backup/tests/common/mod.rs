//! Helpers of the repository tests: small databases, sources of closed database
//! directories, repositories on `memory://` and tempdir `fs`, and wrapper stores that
//! inject backend behavior.

#![allow(dead_code)]

use async_trait::async_trait;
use futures::stream::BoxStream;
use object_store::memory::InMemory;
use object_store::path::Path as Key;
use object_store::{
    CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMode, PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use sparkles_backup::{CreateOptions, OpenEnv, RepoConfig, Repository, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{CapturedFile, FileKind, FileSource, LeaseGuard, Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

/// A persistent database at `dir` with the commits of the acceptance setup: 1 inserts
/// `<urn:a>`, 2 inserts `<urn:b>`, 3 deletes `<urn:a>`. Closed on return.
pub fn make_db(dir: &Path) {
    let s = Store::open(dir, StoreOptions::default()).unwrap();
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
    upd(&s, "DELETE DATA { <urn:a> <urn:p> 1 }");
}

/// Open the database at `dir`, apply `u`, close it.
pub fn commit(dir: &Path, u: &str) {
    let s = Store::open(dir, StoreOptions::default()).unwrap();
    upd(&s, u);
}

/// A backup source for the closed database directory `dir`.
pub fn closed_source(dir: &Path) -> Source {
    Source::from_closed_dir(dir).unwrap()
}

pub fn memory_config(name: &str) -> RepoConfig {
    RepoConfig::from_url(name, "memory://").unwrap()
}

/// A repository over `store` (a shared `InMemory` or a wrapper), no cache directory.
pub async fn open_on(store: Arc<dyn ObjectStore>, cfg: &RepoConfig) -> Repository {
    Repository::open(
        cfg,
        &OpenEnv {
            store: Some(store),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

pub async fn memory_repo() -> (Repository, Arc<InMemory>) {
    let mem = Arc::new(InMemory::new());
    (open_on(mem.clone(), &memory_config("mem")).await, mem)
}

/// A source of made-up files (not a database: for the upload mechanics only), of a
/// fresh dataset id at commit 1 of `gen-0001`.
pub fn synthetic(files: Vec<(&str, FileKind, Vec<u8>)>) -> Source {
    Source {
        next_ordinal: 1,
        branch: None,
        dataset_id: uuid::Uuid::new_v4(),
        commit: sparkles_core::commit::CommitInfo {
            seq: 1,
            timestamp_ms: 1_790_000_000_000,
            kind: sparkles_core::commit::CommitKind::Update,
            inserted: 1,
            deleted: 0,
            quads: 1,
            generation: 1,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        },
        generation: "gen-0001".into(),
        branches_omitted: 0,
        index_format: sparkles_core::builder::FORMAT_VERSION,
        files: files
            .into_iter()
            .map(|(path, kind, bytes)| CapturedFile {
                path: path.to_string(),
                kind,
                len: bytes.len() as u64,
                src: FileSource::Bytes(Arc::from(bytes)),
            })
            .collect(),
        lock_hold: Duration::ZERO,
        lease: LeaseGuard::none(),
        in_memory: false,
    }
}

/// `n` incompressible bytes (xorshift), different for each `seed`.
pub fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

pub fn opts(name: &str, dataset: &str) -> CreateOptions {
    CreateOptions {
        name: name.to_string(),
        dataset_name: dataset.to_string(),
        ..Default::default()
    }
}

/// Counts the objects under `prefix`.
pub async fn count(store: &dyn ObjectStore, prefix: &str) -> usize {
    use futures::TryStreamExt;
    store
        .list(Some(&Key::from(prefix)))
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .len()
}

/// What a [`Faulty`] store does with a put.
pub type PutHook = dyn Fn(&Key, &PutOptions) -> Option<object_store::Error> + Send + Sync;

/// A store that forwards to `inner`, except that `hook` may fail a put first (the
/// fault-injection wrapper of the backend tests), and conditional creates may be
/// ignored (`ignore_create`: a service without `If-None-Match`) or refused as not
/// implemented (`no_create`: S3 with conditional puts disabled), and "already exists"
/// may be answered as a precondition failure (`precondition`: 412).
pub struct Faulty {
    pub inner: Arc<dyn ObjectStore>,
    pub hook: Option<Box<PutHook>>,
    pub ignore_create: bool,
    pub no_create: bool,
    pub precondition: bool,
    /// puts of `backups/*` wait until this many have arrived (two concurrent writers
    /// racing for one name)
    pub manifest_barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl Faulty {
    pub fn new(inner: Arc<dyn ObjectStore>) -> Faulty {
        Faulty {
            inner,
            hook: None,
            ignore_create: false,
            no_create: false,
            precondition: false,
            manifest_barrier: None,
        }
    }
}

impl std::fmt::Display for Faulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Faulty({})", self.inner)
    }
}

impl std::fmt::Debug for Faulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Faulty({})", self.inner)
    }
}

pub fn generic(msg: &str) -> object_store::Error {
    object_store::Error::Generic {
        store: "faulty",
        source: msg.to_string().into(),
    }
}

#[async_trait]
impl ObjectStore for Faulty {
    async fn put_opts(
        &self,
        location: &Key,
        payload: PutPayload,
        mut opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        if let Some(h) = &self.hook
            && let Some(e) = h(location, &opts)
        {
            return Err(e);
        }
        if let Some(b) = &self.manifest_barrier
            && location.as_ref().starts_with("backups/")
        {
            b.wait().await;
        }
        if opts.mode == PutMode::Create {
            if self.no_create {
                return Err(object_store::Error::NotImplemented {
                    operation: "conditional put".into(),
                    implementer: "faulty".into(),
                });
            }
            if self.ignore_create {
                opts.mode = PutMode::Overwrite;
            }
        }
        match self.inner.put_opts(location, payload, opts).await {
            Err(object_store::Error::AlreadyExists { path, source }) if self.precondition => {
                Err(object_store::Error::Precondition { path, source })
            }
            r => r,
        }
    }

    async fn put_multipart_opts(
        &self,
        location: &Key,
        opts: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(
        &self,
        location: &Key,
        options: GetOptions,
    ) -> object_store::Result<GetResult> {
        self.inner.get_opts(location, options).await
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, object_store::Result<Key>>,
    ) -> BoxStream<'static, object_store::Result<Key>> {
        self.inner.delete_stream(locations)
    }

    fn list(&self, prefix: Option<&Key>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(&self, prefix: Option<&Key>) -> object_store::Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
        &self,
        from: &Key,
        to: &Key,
        options: CopyOptions,
    ) -> object_store::Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
}
