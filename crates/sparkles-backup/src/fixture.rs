//! Test fixtures: repositories built directly on a store (marker, blobs and manifests
//! laid out with `layout` and `blob`), without the attach and create code paths.
#![allow(dead_code)] // shared by the test modules; each uses some

use crate::blob;
use crate::cache::ManifestCache;
use crate::layout::{self, Marker};
use crate::throttle::Throttle;
use crate::{
    BlobRef, Derived, FileEntry, Manifest, ManifestCommit, ManifestDataset, ManifestStats, OpenEnv,
    RepoConfig, RepoType, Repository, ServerInfo, TextDerived,
};
use futures::stream::BoxStream;
use object_store::memory::InMemory;
use object_store::path::Path as Key;
use object_store::{
    CopyOptions, GetOptions, GetResult, GetResultPayload, ListResult, MultipartUpload, ObjectMeta,
    ObjectStore, ObjectStoreExt, PutMode, PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{FileKind, Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use uuid::Uuid;

/// The piece size of fixture repositories: small, so index files span several blobs.
pub const PIECE: u64 = 4096;

pub fn memory_store() -> Arc<dyn ObjectStore> {
    Arc::new(InMemory::new())
}

/// A repository on `store` (its marker written if absent), with `f` applied to the
/// configuration.
pub async fn repo_with(store: Arc<dyn ObjectStore>, f: impl FnOnce(&mut RepoConfig)) -> Repository {
    let marker = match store.get(&layout::marker_key()).await {
        Ok(g) => Marker::parse(&g.bytes().await.unwrap()).unwrap(),
        Err(_) => {
            let mut m = Marker::new(Uuid::new_v4(), crate::now_rfc3339());
            m.piece_bytes = PIECE;
            store
                .put(&layout::marker_key(), PutPayload::from(m.to_bytes()))
                .await
                .unwrap();
            m
        }
    };
    let mut config = RepoConfig {
        name: "local".into(),
        kind: RepoType::Memory,
        conditional_writes: true,
        ..Default::default()
    };
    f(&mut config);
    Repository {
        store: store.clone(),
        config,
        marker,
        env: OpenEnv {
            store: Some(store),
            ..Default::default()
        },
        cache: ManifestCache::new(None),
        upload: Throttle::unlimited(),
        download: Throttle::unlimited(),
        requests: Default::default(),
        conditional: std::sync::atomic::AtomicU8::new(crate::repo::COND_UNKNOWN),
    }
}

pub async fn memory_repo() -> Repository {
    repo_with(memory_store(), |_| {}).await
}

/// An `fs` repository in `dir`.
pub async fn fs_repo(dir: &Path) -> Repository {
    std::fs::create_dir_all(dir).unwrap();
    let store = object_store::local::LocalFileSystem::new_with_prefix(dir)
        .unwrap()
        .with_fsync(true);
    repo_with(Arc::new(store), |c| {
        c.kind = RepoType::Fs;
        c.path = Some(dir.display().to_string());
    })
    .await
}

pub fn upd(s: &Store, text: &str) {
    update(s, text, &QueryOptions::default()).unwrap();
}

/// The acceptance setup: a database at `root` with commits 1 `INSERT <urn:a>`, 2
/// `INSERT <urn:b>`, 3 `DELETE <urn:a>`. Returns its dataset id.
pub fn make_db(root: &Path) -> Uuid {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
    upd(&s, "DELETE DATA { <urn:a> <urn:p> 1 }");
    s.dataset_id()
}

/// A larger database at `root`: the acceptance commits, 3000 quads in commit 4, a
/// compaction (`gen-0002`, whose index files span several pieces), and commit 5
/// `INSERT <urn:c>`. Head 5, 3002 quads.
pub fn make_big_db(root: &Path) -> Uuid {
    let id = make_db(root);
    let s = Store::open(root, StoreOptions::default()).unwrap();
    let mut q = String::from("INSERT DATA {");
    for i in 0..3000 {
        q.push_str(&format!(" <urn:s{i}> <urn:p{}> \"value {i}\" .", i % 7));
    }
    q.push('}');
    upd(&s, &q);
    s.compact().unwrap();
    upd(&s, "INSERT DATA { <urn:c> <urn:p> 3 }");
    id
}

/// The files a backup of the closed database `root` holds, with their kinds.
pub fn backup_files(root: &Path) -> Vec<(String, FileKind)> {
    let generation = std::fs::read_to_string(root.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(root.join(&generation))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| !n.ends_with(".tmp") && !n.ends_with(".deleting"))
        .collect();
    names.sort();
    for n in names {
        let kind = if n == "wal.log" || n == "delta.vocab" {
            FileKind::Append
        } else {
            FileKind::Immutable
        };
        out.push((format!("{generation}/{n}"), kind));
    }
    out.push(("commits.bin".into(), FileKind::Append));
    for n in layout::ROOT_FILES {
        if n != "commits.bin" && root.join(n).is_file() {
            out.push((n.to_string(), FileKind::Meta));
        }
    }
    out
}

/// Back up the closed database `root` as `name` by laying out its blobs and manifest
/// directly (every file stored whole, as pieces).
pub async fn put_backup(repo: &Repository, root: &Path, name: &str) -> Manifest {
    let (id, head) = {
        let s = Store::open(root, StoreOptions::default()).unwrap();
        (s.dataset_id(), s.head_commit())
    };
    let generation = std::fs::read_to_string(root.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let t0 = crate::now_rfc3339();
    let mut stats = ManifestStats::default();
    let mut files = Vec::new();
    for (path, kind) in backup_files(root) {
        let data = std::fs::read(root.join(&path)).unwrap();
        let mut blobs = Vec::new();
        for (off, len) in blob::pieces(data.len() as u64, repo.marker.piece_bytes) {
            let plain = &data[off as usize..(off + len) as usize];
            let e = blob::encode(plain, true);
            match repo
                .store
                .put_opts(
                    &layout::blob_key(&e.id),
                    PutPayload::from(e.bytes.clone()),
                    PutMode::Create.into(),
                )
                .await
            {
                Ok(_) => {
                    stats.new_blobs += 1;
                    stats.added_bytes += e.bytes.len() as u64;
                }
                Err(object_store::Error::AlreadyExists { .. }) => stats.reused_blobs += 1,
                Err(e) => panic!("{e}"),
            }
            stats.blobs += 1;
            blobs.push(BlobRef {
                id: e.id,
                size: len,
            });
        }
        stats.files += 1;
        stats.logical_bytes += data.len() as u64;
        files.push(FileEntry {
            path,
            kind,
            size: data.len() as u64,
            sha256: blob::blob_id(&data),
            blobs,
        });
    }
    let m = Manifest {
        format: 1,
        kind: layout::MANIFEST_KIND.into(),
        name: name.into(),
        id: Uuid::new_v4(),
        repository_id: repo.id(),
        dataset: ManifestDataset {
            next_ordinal: None,
            branch: None,
            name: "ds".into(),
            id,
            kind: "persistent".into(),
        },
        commit: ManifestCommit::from(&head),
        generation,
        index_format: sparkles_core::builder::FORMAT_VERSION,
        created: t0,
        completed: crate::now_rfc3339(),
        millis: 1,
        server: ServerInfo {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        parent: None,
        policy: None,
        run: None,
        note: None,
        files,
        stats,
        derived: Derived {
            text: root.join("text.json").is_file().then_some(TextDerived {
                rebuild_on_restore: true,
            }),
        },
        encryption: None,
        branches_omitted: 0,
    };
    put_manifest(repo, &m).await;
    m
}

/// Store `m` as `backups/<name>.json` (overwriting: tests plant hostile manifests).
pub async fn put_manifest(repo: &Repository, m: &Manifest) {
    repo.store
        .put(
            &layout::manifest_key(&m.name),
            PutPayload::from(serde_json::to_vec_pretty(m).unwrap()),
        )
        .await
        .unwrap();
}

/// Every blob id a manifest references.
pub fn blob_ids(m: &Manifest) -> std::collections::BTreeSet<String> {
    m.files
        .iter()
        .flat_map(|f| f.blobs.iter().map(|b| b.id.clone()))
        .collect()
}

/// Replace the stored object of blob `id` with `f` applied to its bytes.
pub async fn tamper_blob(repo: &Repository, id: &str, f: impl FnOnce(&mut Vec<u8>)) {
    let key = layout::blob_key(id);
    let mut b = repo
        .store
        .get(&key)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    f(&mut b);
    repo.store.put(&key, PutPayload::from(b)).await.unwrap();
}

/// A store that damages the next `corrupt` reads of one key (flips the last byte) and
/// counts every `GET`; everything else goes to `inner`.
#[derive(Debug)]
pub struct Flaky {
    pub inner: Arc<dyn ObjectStore>,
    pub target: std::sync::Mutex<Option<Key>>,
    pub corrupt: AtomicUsize,
    pub gets: AtomicUsize,
}

impl Flaky {
    pub fn new(inner: Arc<dyn ObjectStore>) -> Flaky {
        Flaky {
            inner,
            target: Default::default(),
            corrupt: Default::default(),
            gets: Default::default(),
        }
    }
}

impl std::fmt::Display for Flaky {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Flaky({})", self.inner)
    }
}

#[async_trait::async_trait]
impl ObjectStore for Flaky {
    async fn put_opts(
        &self,
        location: &Key,
        payload: PutPayload,
        opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.inner.put_opts(location, payload, opts).await
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
        self.gets.fetch_add(1, SeqCst);
        let r = self.inner.get_opts(location, options).await?;
        let hit = self.target.lock().unwrap().as_ref() == Some(location);
        if !hit
            || self
                .corrupt
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                .is_err()
        {
            return Ok(r);
        }
        let (meta, range, attributes, extensions) = (
            r.meta.clone(),
            r.range.clone(),
            r.attributes.clone(),
            r.extensions.clone(),
        );
        let mut b = r.bytes().await?.to_vec();
        let last = b.len() - 1;
        b[last] ^= 1;
        let b = bytes::Bytes::from(b);
        Ok(GetResult {
            payload: GetResultPayload::Stream(Box::pin(futures::stream::once(
                async move { Ok(b) },
            ))),
            meta,
            range,
            attributes,
            extensions,
        })
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
