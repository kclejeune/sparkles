//! A verified replacement and its commit-bound manifest.

use super::recipe::{self, Document, DocumentKind, Format, Plan, Recipe};
use super::{CatalogFile, Pin};
use crate::auth::{Endpoint, Level, Principal};
use crate::http::{ApiResult, BodyBudget, err_code};
use crate::state::{AppState, Dataset};
use axum::http::StatusCode;
use oxrdf::NamedNode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sparkles::commit::{CommitKind, Receipt};
use sparkles::guard::{CommitIntent, WriteOptions};
use sparkles::io::{RdfSyntax, Source, SourceData};
use sparkles::outbound::OutboundPolicy;
use sparkles::store::{ReplaceTarget, Store};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use sparkles::store::source_manifests::{ANNOTATION, DIRECTORY};

const STAGING: &str = ".source-load-staging";
const DOWNLOAD_LIMIT: u64 = 64 << 30;
const DOWNLOAD_TIME: Duration = Duration::from_secs(6 * 3600);
const MAX_MANIFEST_BYTES: usize = 4 << 20;
const MAX_MEMORY_MANIFESTS: usize = 128;
const MAX_MEMORY_BYTES: usize = 16 << 20;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Manifest {
    format: Format,
    kind: DocumentKind,
    /// Distinguishes two invocations of the same recipe on the same store.
    id: Uuid,
    pub dataset_id: Uuid,
    pub recipe: Recipe,
    pub plan: Plan,
    pub reloaded_from: Option<u64>,
    pub by: String,
}

impl Document for Manifest {
    const KIND: DocumentKind = DocumentKind::LoadManifest;
}

type MemoryRecords = BTreeMap<(Uuid, u64), Vec<u8>>;

#[derive(Default)]
pub(crate) struct MemoryManifests {
    records: parking_lot::Mutex<MemoryRecords>,
    admission: parking_lot::Mutex<()>,
}

struct Intent<'a> {
    store: &'a Store,
    memory: &'a MemoryManifests,
    bytes: Vec<u8>,
}

fn location(root: &Path, seq: u64) -> PathBuf {
    root.join(DIRECTORY).join(format!("{seq}.json"))
}

impl CommitIntent for Intent<'_> {
    fn persist(&self, seq: u64) -> sparkles::Result<()> {
        if let Some(root) = self.store.root() {
            let path = location(root, seq);
            sparkles::fsutil::write_atomic(&path, &self.bytes)?;
        } else {
            let mut manifests = self.memory.records.lock();
            manifests.insert((self.store.dataset_id(), seq), self.bytes.clone());
        }
        Ok(())
    }

    fn retire(&self, seq: u64) -> sparkles::Result<()> {
        // Retirement runs only after a definite refusal before publication.
        if let Some(root) = self.store.root() {
            match fs::remove_file(location(root, seq)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            sparkles::fsutil::sync_dir(&root.join(DIRECTORY))?;
        } else {
            self.memory
                .records
                .lock()
                .remove(&(self.store.dataset_id(), seq));
        }
        Ok(())
    }
}

fn missing(seq: u64) -> crate::http::ApiError {
    err_code(
        StatusCode::NOT_FOUND,
        "no-source-manifest",
        format!("commit {seq} has no source manifest"),
    )
}

fn unreadable(detail: impl std::fmt::Display) -> crate::http::ApiError {
    err_code(
        StatusCode::SERVICE_UNAVAILABLE,
        "source-manifest-unreadable",
        format!("the source manifest cannot be read: {detail}"),
    )
}

pub(crate) fn read(st: &AppState, ds: &Dataset, seq: u64) -> ApiResult<Manifest> {
    let store = &ds.store;
    if seq > store.head_commit().seq {
        return Err(missing(seq));
    }
    let annotation = store.annotation(seq).ok_or_else(|| missing(seq))?;
    let digest = annotation
        .message
        .as_deref()
        .and_then(|message| message.strip_prefix(ANNOTATION))
        .ok_or_else(|| missing(seq))?;
    let bytes = if let Some(root) = store.root() {
        let mut bytes = Vec::new();
        let file = File::open(location(root, seq)).map_err(unreadable)?;
        file.take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(unreadable)?;
        bytes
    } else {
        st.resources
            .loads
            .records
            .lock()
            .get(&(store.dataset_id(), seq))
            .cloned()
            .ok_or_else(|| missing(seq))?
    };
    if bytes.len() > MAX_MANIFEST_BYTES || recipe::hash(&bytes) != digest {
        return Err(unreadable("the content does not match the commit digest"));
    }
    let manifest: Manifest = recipe::decode(&bytes).map_err(|e| unreadable(super::message(&e)))?;
    if manifest.dataset_id != store.dataset_id() {
        return Err(missing(seq));
    }
    Ok(manifest)
}

fn forbidden(message: &str) -> crate::http::ApiError {
    err_code(StatusCode::FORBIDDEN, "forbidden", message)
}

/// Sidecar provenance describes whole graphs. A partial triple view would see a
/// different load and must not learn the existence, source or size of hidden data.
pub(crate) fn allow_graphs(p: &Principal, ds: &Dataset, plan: &Plan, write: bool) -> ApiResult<()> {
    let need = if write { Level::Write } else { Level::Read };
    if !p.can_at(&ds.name, Endpoint::Query, Level::Read)
        || (write && !p.can_at(&ds.name, Endpoint::Upload, need))
    {
        return Err(forbidden(
            "source loads need Query access and replacements need Upload access",
        ));
    }
    let reads = p.view(&ds.name, Endpoint::Query);
    let writes = write.then(|| p.view(&ds.name, Endpoint::Upload)).flatten();
    for graph in plan.files.iter().map(|f| f.graph.as_str()) {
        if reads
            .as_ref()
            .is_some_and(|v| v.hides_triples() || !v.read.allows_named_iri(graph))
            || writes
                .as_ref()
                .is_some_and(|v| v.triples.is_some() || !v.writable_iri(graph))
        {
            return Err(forbidden(
                "source loads require full access to each target graph",
            ));
        }
    }
    Ok(())
}

fn check_terms(catalog: &CatalogFile, plan: &Plan) -> ApiResult<()> {
    let pins: BTreeSet<&Pin> = plan.files.iter().map(|f| &f.source).collect();
    for pin in pins {
        let report = super::report::license_report(
            catalog,
            &pin.resource,
            Some(&pin.version),
            super::license::Policy::Warn,
        )?;
        if !report.unaccepted.is_empty() {
            return Err(err_code(
                StatusCode::FORBIDDEN,
                "license-unaccepted",
                format!(
                    "restricted terms need acceptance for {}",
                    report.unaccepted.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn fetch_error(detail: impl std::fmt::Display) -> crate::http::ApiError {
    err_code(
        StatusCode::BAD_GATEWAY,
        "source-fetch-failed",
        detail.to_string(),
    )
}

fn download_error(e: sparkles::Error) -> crate::http::ApiError {
    match e {
        sparkles::Error::Timeout => e.into(),
        other => fetch_error(other),
    }
}

pub(crate) fn clean_staging(data_dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(data_dir.join(STAGING)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn prepare_manifests(root: &Path) -> io::Result<()> {
    match fs::create_dir(root.join(DIRECTORY)) {
        Ok(()) => sparkles::fsutil::sync_dir(root),
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(root.join(DIRECTORY))?.is_dir() =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn until_cancelled<T>(
    future: impl Future<Output = T>,
    opts: &WriteOptions,
) -> sparkles::Result<T> {
    tokio::pin!(future);
    loop {
        opts.check()?;
        tokio::select! {
            result = &mut future => return Ok(result),
            () = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

fn spool(st: &AppState, plan: &Plan, dir: &Path, opts: &WriteOptions) -> ApiResult<Vec<Source>> {
    let policy = OutboundPolicy {
        timeout: DOWNLOAD_TIME,
        request_timeout: DOWNLOAD_TIME,
        max_response_bytes: DOWNLOAD_LIMIT,
        max_request_bytes: DOWNLOAD_LIMIT,
        ..st.outbound.clone()
    };
    let deadline = Instant::now() + policy.request_timeout;
    let deadline = opts.deadline.map_or(deadline, |d| d.min(deadline));
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| unreadable("the source download runtime is unavailable"))?;
    let mut disk = BodyBudget::new(&st.limits);
    let mut total = 0u64;
    let mut sources = Vec::with_capacity(plan.files.len());
    let mut staged = BTreeMap::new();
    for file in &plan.files {
        opts.check()?;
        let path = dir.join(&file.sha256);
        // Each URL in the manifest is verified, even when mirrors share a digest.
        // Equal references reuse the same checked file within this request.
        let key = (&file.url, &file.sha256);
        let size = if let Some(size) = staged.get(&key) {
            *size
        } else {
            let mut response = runtime
                .block_on(until_cancelled(
                    policy.open_download(&file.url, deadline),
                    opts,
                ))?
                .map_err(download_error)?;
            if response.status() != StatusCode::OK {
                return Err(fetch_error(format!(
                    "source {} returned HTTP {}",
                    file.url,
                    response.status()
                )));
            }
            let part = path.with_extension("part");
            let mut dest = File::create(&part).map_err(sparkles::Error::from)?;
            let mut digest = Sha256::new();
            let mut size = 0u64;
            while let Some(chunk) = runtime
                .block_on(until_cancelled(response.chunk(), opts))?
                .map_err(download_error)?
            {
                let chunk = chunk.as_ref();
                let n = chunk.len();
                size += n as u64;
                total += n as u64;
                if size > policy.max_response_bytes || total > policy.max_request_bytes {
                    return Err(fetch_error("the source download byte limit was exceeded"));
                }
                if file.byte_size.is_some_and(|expected| size > expected) {
                    return Err(fetch_error(format!(
                        "source {} exceeded its pinned size",
                        file.url
                    )));
                }
                disk.read(n)?;
                disk.disk(dir, n)?;
                digest.update(chunk);
                dest.write_all(chunk).map_err(sparkles::Error::from)?;
            }
            if file.sha256 != recipe::hex(&digest.finalize()) {
                return Err(fetch_error(format!(
                    "source {} did not match its pinned checksum",
                    file.url
                )));
            }
            drop(dest);
            fs::rename(part, &path).map_err(sparkles::Error::from)?;
            staged.insert(key, size);
            size
        };
        if file.byte_size.is_some_and(|expected| size != expected) {
            return Err(fetch_error(format!(
                "source {} did not match its pinned size",
                file.url
            )));
        }
        let graph = NamedNode::new(&file.graph).map_err(|e| super::invalid(e.to_string()))?;
        let format = RdfSyntax::from_name(&file.rdf_format)
            .ok_or_else(|| super::invalid("unrecognized RDF format in source plan"))?;
        let mut source = Source::from_bytes(Vec::new(), format, Some(graph));
        source.data = SourceData::File(path);
        source.name.clone_from(&file.url);
        source.base = Some(file.url.clone());
        source.max_decompressed = st.limits.max_decompressed_bytes;
        source.triples_only = true;
        sources.push(source);
    }
    Ok(sources)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Loaded {
    pub(crate) receipt: Receipt,
    manifest: Manifest,
}

/// Download and verify the plan, recheck catalog terms, and replace the graphs once.
pub(crate) fn execute(
    st: &Arc<AppState>,
    ds: &Dataset,
    p: &Principal,
    recipe: Recipe,
    previous: Option<(u64, Plan)>,
    mut opts: WriteOptions,
) -> ApiResult<Loaded> {
    if ds.branch.is_some() {
        return Err(super::invalid("catalog loads use the main branch").into());
    }
    let stop = sparkles::task::Cancellation::from_flags([st.resources.stop.clone()]);
    opts.cancellation = Some(
        opts.cancellation
            .as_ref()
            .map_or_else(|| stop.clone(), |c| c.combine(&stop)),
    );
    let catalog = st.resources.read()?;
    let plan = recipe::plan(&catalog, &recipe)?;
    allow_graphs(p, ds, &plan, true)?;
    if previous.as_ref().is_some_and(|(_, old)| old != &plan) {
        return Err(err_code(
            StatusCode::CONFLICT,
            "source-plan-changed",
            "the catalog no longer resolves to the same pinned files",
        ));
    }
    check_terms(&catalog, &plan)?;
    drop(catalog);

    let data_dir = if st.data_dir.as_os_str().is_empty() {
        ds.store
            .root()
            .ok_or_else(|| super::invalid("catalog loads need a data directory"))?
    } else {
        st.data_dir.as_path()
    };
    let staging = data_dir.join(STAGING);
    fs::create_dir_all(&staging).map_err(sparkles::Error::from)?;
    let dir = tempfile::Builder::new()
        .prefix("load-")
        .tempdir_in(&staging)
        .map_err(sparkles::Error::from)?;
    if let Some(root) = ds.store.root() {
        prepare_manifests(root).map_err(sparkles::Error::from)?;
    }
    let sources = spool(st, &plan, dir.path(), &opts)?;
    let graphs = sources
        .iter()
        .map(|s| s.graph.clone())
        .collect::<BTreeSet<_>>();
    let manifest = Manifest {
        format: Format,
        kind: DocumentKind::LoadManifest,
        id: Uuid::new_v4(),
        dataset_id: ds.store.dataset_id(),
        recipe,
        plan,
        reloaded_from: previous.map(|(seq, _)| seq),
        by: p.id(),
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|e| super::Error::Io(e.to_string()))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(super::Error::Limit("the manifest exceeds 4 MiB".into()).into());
    }
    opts.message = Some(format!("{ANNOTATION}{}", recipe::hash(&bytes)).into());
    // Every fixed target was checked above. These sources accept only single-graph RDF,
    // so the WriteOptions graph mask is unnecessary and would force a transactional load.
    opts.graphs = None;
    let authority = st.clone();
    let principal = p.clone();
    opts.before_publish = Some(sparkles::guard::PublishHook::new(move || {
        if authority.access_current(&principal) {
            Ok(())
        } else {
            Err(sparkles::Error::NotPermitted(
                "the caller's authorization changed during the load".into(),
            ))
        }
    }));
    let intent = Intent {
        store: &ds.store,
        memory: &st.resources.loads,
        bytes,
    };
    let current = st.resources.read()?;
    if recipe::plan(&current, &manifest.recipe)? != manifest.plan {
        return Err(err_code(
            StatusCode::CONFLICT,
            "source-plan-changed",
            "the catalog changed during the load",
        ));
    }
    check_terms(&current, &manifest.plan)?;
    drop(current);
    let _admission = if ds.store.root().is_none() {
        let admission = st.resources.loads.admission.lock();
        let mut live = BTreeMap::new();
        for dataset in st
            .datasets()
            .into_values()
            .filter(|dataset| dataset.store.root().is_none())
        {
            opts.check()?;
            let first = dataset
                .store
                .commits(sparkles::commit::CommitRange::Latest, 1)
                .first_retained;
            live.insert(dataset.store.dataset_id(), first);
        }
        let mut manifests = st.resources.loads.records.lock();
        manifests.retain(|(id, seq), _| live.get(id).is_some_and(|first| seq >= first));
        if manifests.len() >= MAX_MEMORY_MANIFESTS
            || manifests.values().map(Vec::len).sum::<usize>() + intent.bytes.len()
                > MAX_MEMORY_BYTES
        {
            return Err(super::Error::Limit(
                "the in-memory source manifest limit is reached".into(),
            )
            .into());
        }
        Some(admission)
    } else {
        None
    };
    let prefixes = crate::settings::resolved(st, ds, &crate::settings::prefixes::PREFIXES);
    let mut install = BTreeMap::new();
    for (prefix, iri) in &manifest.plan.prefixes {
        match prefixes
            .effective
            .get(prefix)
            .and_then(serde_json::Value::as_str)
        {
            Some(old) if old != iri => {
                return Err(err_code(
                    StatusCode::CONFLICT,
                    "prefix-conflict",
                    format!("prefix {prefix} is already bound to {old}"),
                ));
            }
            Some(_)
                if crate::settings::prefixes::keeps(&st.settings.declared(), &ds.name, prefix) => {}
            _ => {
                install.insert(prefix.clone(), iri.clone());
            }
        }
    }
    let receipt = ds
        .store
        .replace_with_intent(
            ReplaceTarget::Graphs(graphs.into_iter().collect()),
            &sources,
            CommitKind::Load,
            &opts,
            &intent,
            install,
        )?
        .1;
    Ok(Loaded { receipt, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::store::StoreOptions;

    #[test]
    fn a_failed_manifest_write_refuses_publication_until_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        prepare_manifests(dir.path()).unwrap();
        let head = store.head_commit().seq;
        let path = location(dir.path(), head + 1);
        let blocked = path.with_extension("tmp");
        fs::create_dir(&blocked).unwrap();
        let memory = MemoryManifests::default();
        let intent = Intent {
            store: &store,
            memory: &memory,
            bytes: b"manifest".to_vec(),
        };
        let input = Source::from_bytes(
            b"<urn:s> <urn:p> <urn:o> .".to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        );
        let failed = store.replace_with_intent(
            ReplaceTarget::Default,
            std::slice::from_ref(&input),
            CommitKind::Load,
            &WriteOptions::default(),
            &intent,
            BTreeMap::new(),
        );
        assert!(failed.is_err());
        assert!(!path.exists());
        assert_eq!(store.head_commit().seq, head);
        assert!(matches!(
            store.try_write_with(CommitKind::Transaction, WriteOptions::default()),
            Err(sparkles::Error::Poisoned)
        ));
        drop(store);
        fs::remove_dir(blocked).unwrap();

        let reopened = Store::open(dir.path(), StoreOptions::default()).unwrap();
        assert_eq!(reopened.head_commit().seq, head);
        reopened.load(&[input]).unwrap();
        assert_eq!(reopened.head_commit().seq, head + 1);
    }
}
