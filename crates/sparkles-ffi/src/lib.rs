//! The native library of the JVM bindings (spec P04): the engine exported through UniFFI.
//!
//! Every call that moves RDF terms moves a batch of them in the encoding of
//! [`encode`], so a call across the boundary is paid once per batch. The objects are
//! [`FfiDataset`], [`FfiReadTxn`], [`FfiWriteTxn`], [`FfiQuery`] and [`FfiCursor`]. The
//! Kotlin library `sparkles-jena` builds Jena's `DatasetGraph` on them.

uniffi::setup_scaffolding!();

mod admin;
pub mod encode;
mod error;
mod patch;
pub use patch::PatchReport;
mod advanced;
mod backups;
mod branches;
mod catalog;
mod documents;
mod helpers;
pub use helpers::{
    DiagnosticsRequest, EmbeddingEnvironment, GeoFeaturesRequest, RecallRequest, TextSearchRequest,
    check_data, check_iri, check_langtag, convert_geometries, format_document, lint_document,
    preview_schedule,
};
mod history;
mod settings;
pub use admin::{
    DescribeSettings, FfiDump, FfiOperation, OperationProgress, SnapshotInfo, TextInfo,
    TextSettings,
};
pub use advanced::{
    GeoInfo, GeoSettings, GuardInfo, GuardSettings, ReasonInfo, ReasonSettings, ShaclReport,
    ShaclResult, ShexReport, ShexResult, VectorInfo, VectorSettings,
};
pub use backups::{
    BackupInfo, BackupVerification, FfiBackupRepository, GcInfo, LockInfo, RepositoryTest,
    VerifyInfo,
};
pub use branches::{BranchInfo, ConflictCell, MergeInfo, MergeSettings};
pub use catalog::{CatalogFile, DatasetInfo, FfiCatalog, FfiReservation, catalog_inspect};
pub use documents::{BoundQuery, QueryChange, SchemaRequest};
pub use settings::{CompactionSettings, QuotaInfo, RetentionSettings, SnapshotSchedule};
mod cursor;
mod jni_calls;
mod labels;
mod query;
mod read;

pub use cursor::FfiSelectCursor;
pub use error::{ErrorKind, FfiError, FfiResult};
pub use query::{Execution, FfiQuery, FfiQueryKind, QueryOpts, Timing};
pub use read::{Batch, FfiCursor, FindResult};

use encode::{Item, Reader};
use labels::{LabelTable, Labels};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use parking_lot::{Mutex, RwLock};
use read::SnapReader;
use sparkles::Dataset;
use sparkles::embed::TxnWorker;
use sparkles::id::Id;
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Snapshot, StoreOptions};
use std::collections::HashMap;
use std::sync::Arc;

/// The library's version and the version of its batch encoding, which the Kotlin side
/// checks against its own when it loads the library.
#[derive(uniffi::Record)]
pub struct VersionInfo {
    pub crate_version: String,
    pub encoding_version: u32,
}

#[uniffi::export]
pub fn ffi_version() -> VersionInfo {
    VersionInfo {
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
        encoding_version: encode::ENCODING_VERSION,
    }
}

/// How blank node labels from Jena map to stored nodes (P04 §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum BlankNodeMode {
    /// a label written in a committed transaction names its node for as long as the
    /// dataset is open
    Dataset,
    /// labels are scoped to the transaction that writes them
    Transaction,
}

#[derive(Clone, uniffi::Record)]
pub struct DatasetOptions {
    pub blank_node_labels: BlankNodeMode,
    /// refuse every write
    pub read_only: bool,
    /// the most terms a result's term table holds before it restarts
    pub term_cache_size: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct CommitInfo {
    pub seq: u64,
    pub timestamp_ms: i64,
    pub kind: String,
    pub inserted: u64,
    pub deleted: u64,
    pub quads: u64,
}

impl From<&sparkles::commit::CommitInfo> for CommitInfo {
    fn from(c: &sparkles::commit::CommitInfo) -> CommitInfo {
        CommitInfo {
            seq: c.seq,
            timestamp_ms: c.timestamp_ms,
            kind: format!("{:?}", c.kind).to_lowercase(),
            inserted: c.inserted,
            deleted: c.deleted,
            quads: c.quads,
        }
    }
}

/// What a commit did. `committed` is false when the transaction changed nothing, and
/// `commit` is then the unchanged head.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Receipt {
    pub committed: bool,
    pub commit: CommitInfo,
    pub dataset_id: String,
}

impl From<&sparkles::commit::Receipt> for Receipt {
    fn from(r: &sparkles::commit::Receipt) -> Receipt {
        Receipt {
            committed: r.committed,
            commit: (&r.commit).into(),
            dataset_id: r.dataset_id.to_string(),
        }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct UpdateStats {
    pub inserted: u64,
    pub deleted: u64,
    pub operations: u64,
    pub total_ms: f64,
}

#[derive(uniffi::Record)]
pub struct ApplyStats {
    pub inserted: u64,
    pub deleted: u64,
}

/// The extension functions, aggregates and property functions this build evaluates, by
/// IRI, and its optional features. The Kotlin fallback detector sends a query to ARQ when
/// it calls a function that Jena has and this list lacks.
#[derive(uniffi::Record)]
pub struct Capabilities {
    pub functions: Vec<String>,
    pub aggregates: Vec<String>,
    pub property_functions: Vec<String>,
    pub features: Vec<String>,
}

struct Inner {
    ds: Dataset,
    labels: Option<Arc<RwLock<LabelTable>>>,
    opts: DatasetOptions,
}

impl Inner {
    fn labels(&self) -> Labels {
        Labels {
            dataset: self.labels.clone(),
            txn: None,
        }
    }

    fn term_cache(&self) -> usize {
        self.opts.term_cache_size.max(1) as usize
    }

    fn check_writable(&self) -> FfiResult<()> {
        if self.opts.read_only {
            return Err(FfiError::new(
                ErrorKind::NotPermitted,
                "the dataset was opened read-only",
            ));
        }
        Ok(())
    }
}

/// One open database.
#[derive(uniffi::Object)]
pub struct FfiDataset {
    inner: Arc<Inner>,
}

impl FfiDataset {
    fn new(ds: Dataset, opts: DatasetOptions) -> Arc<FfiDataset> {
        let labels = (opts.blank_node_labels == BlankNodeMode::Dataset)
            .then(|| Arc::new(RwLock::new(LabelTable::default())));
        Arc::new(FfiDataset {
            inner: Arc::new(Inner { ds, labels, opts }),
        })
    }

    fn head(&self) -> Arc<Snapshot> {
        self.inner.ds.snapshot()
    }

    fn reader<'a>(&'a self, snap: &'a Arc<Snapshot>, labels: &'a Labels) -> SnapReader<'a> {
        SnapReader {
            snap,
            labels,
            term_cache: self.inner.term_cache(),
        }
    }
}

fn graph_iri(graph: Option<String>) -> FfiResult<Option<NamedNode>> {
    graph
        .map(|g| NamedNode::new(g).map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string())))
        .transpose()
}

#[uniffi::export]
impl FfiDataset {
    /// Open or create a persistent database directory.
    #[uniffi::constructor]
    pub fn open(path: String, options: DatasetOptions) -> FfiResult<Arc<FfiDataset>> {
        let ds = Dataset::open_with(&path, StoreOptions::default())?;
        Ok(FfiDataset::new(ds, options))
    }

    /// A new, empty in-memory dataset.
    #[uniffi::constructor]
    pub fn memory(options: DatasetOptions) -> Arc<FfiDataset> {
        FfiDataset::new(Dataset::memory(), options)
    }

    pub fn dataset_id(&self) -> String {
        self.inner.ds.dataset_id().to_string()
    }
    pub fn owner_dataset_id(&self) -> String {
        self.inner.ds.store().owner_dataset_id().to_string()
    }
    /// The native store's directory, for sharing ownership across path and catalog opens.
    pub fn directory(&self) -> Option<String> {
        self.inner
            .ds
            .store()
            .root()
            .map(|p| p.display().to_string())
    }

    /// The commit of the head snapshot, read without the writer lock.
    pub fn head_commit(&self) -> CommitInfo {
        let ds = &self.inner.ds;
        match ds.store().commit(ds.snapshot().commit) {
            Some(c) => (&c).into(),
            None => (&ds.head_commit()).into(),
        }
    }

    pub fn prefixes(&self) -> HashMap<String, String> {
        self.inner.ds.prefixes().into_iter().collect()
    }

    pub fn set_prefix(&self, prefix: String, iri: String) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.set_prefix(&prefix, &iri)?)
    }

    pub fn remove_prefix(&self, prefix: String) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.store().remove_prefix(&prefix)?)
    }

    /// Pin the head snapshot for a read transaction.
    pub fn begin_read(&self) -> Arc<FfiReadTxn> {
        Arc::new(FfiReadTxn {
            ds: self.inner.clone(),
            snap: self.head(),
        })
    }

    /// Begin a write transaction on a worker thread, waiting for the writer lock. With
    /// `expect_commit` it is a promotion: it begins only if the head is still that commit
    /// once the lock is held, and returns `None` otherwise. With `record_labels` false,
    /// the blank node labels it writes never enter the dataset's label table (bulk
    /// loads).
    pub fn begin_write(
        &self,
        expect_commit: Option<u64>,
        record_labels: bool,
    ) -> FfiResult<Option<Arc<FfiWriteTxn>>> {
        self.begin_write_with(expect_commit, record_labels, FfiOperation::new(None))
    }

    /// Begin a writer with cancellation and a deadline while waiting for the lock.
    pub fn begin_write_with(
        &self,
        expect_commit: Option<u64>,
        record_labels: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Option<Arc<FfiWriteTxn>>> {
        self.inner.check_writable()?;
        let opts = sparkles::guard::WriteOptions {
            cancel: Some(operation.control.cancel.flag()),
            deadline: operation.control.deadline,
            ..Default::default()
        };
        let Some(worker) = TxnWorker::begin_with(&self.inner.ds, expect_commit, opts)? else {
            return Ok(None);
        };
        Ok(Some(Arc::new(FfiWriteTxn {
            ds: self.inner.clone(),
            base: worker.base_commit(),
            worker: Mutex::new(Some(worker)),
            view: Mutex::new(None),
            pending: Arc::new(RwLock::new(LabelTable::default())),
            record_labels,
            aborted: Mutex::new(None),
        })))
    }

    /// `find` on the head snapshot, outside a transaction. The cursor pins the snapshot.
    pub fn find(&self, pattern: Vec<u8>, first_rows: u32) -> FfiResult<FindResult> {
        let snap = self.head();
        let labels = self.inner.labels();
        self.reader(&snap, &labels).find(&pattern, first_rows)
    }

    pub fn count(&self, pattern: Vec<u8>) -> FfiResult<u64> {
        let snap = self.head();
        let labels = self.inner.labels();
        self.reader(&snap, &labels).count(&pattern)
    }

    pub fn contains(&self, quad: Vec<u8>) -> FfiResult<bool> {
        let snap = self.head();
        let labels = self.inner.labels();
        self.reader(&snap, &labels).contains(&quad)
    }

    pub fn graph_names(&self) -> FfiResult<Vec<u8>> {
        let snap = self.head();
        let labels = self.inner.labels();
        self.reader(&snap, &labels).graph_names()
    }

    /// A query on the head snapshot.
    pub fn prepare_query(&self, text: String, options: QueryOpts) -> FfiResult<Arc<FfiQuery>> {
        Ok(Arc::new(FfiQuery::prepare(
            self.head(),
            text,
            &options,
            &self.inner.ds,
            self.inner.labels(),
            self.inner.term_cache(),
        )?))
    }

    /// Bulk-load files in one commit; the format is taken from each file's extension.
    /// Triples go to `graph` when it is given. Blank node labels are scoped to the load.
    pub fn load_files(&self, paths: Vec<String>, graph: Option<String>) -> FfiResult<Receipt> {
        self.inner.check_writable()?;
        let g = graph_iri(graph)?;
        let sources = paths
            .iter()
            .map(|p| Source::from_path(std::path::Path::new(p), g.clone()))
            .collect::<sparkles::Result<Vec<_>>>()?;
        Ok((&self.inner.ds.load_sources_receipt(sources)?).into())
    }

    /// Load RDF data in one commit. `format` is a media type or a file extension.
    pub fn load_bytes(
        &self,
        data: Vec<u8>,
        format: String,
        base: Option<String>,
        graph: Option<String>,
    ) -> FfiResult<Receipt> {
        self.inner.check_writable()?;
        let f = RdfFormat::from_media_type(&format)
            .or_else(|| RdfFormat::from_extension(format.trim_start_matches('.')))
            .ok_or_else(|| {
                FfiError::new(
                    ErrorKind::Unsupported,
                    format!("unknown RDF format {format}"),
                )
            })?;
        let mut src = Source::from_bytes(data, f, graph_iri(graph)?);
        src.base = base;
        Ok((&self.inner.ds.load_sources_receipt(vec![src])?).into())
    }

    pub fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    /// The number of blank node labels in the dataset's label table.
    pub fn label_count(&self) -> u64 {
        self.inner
            .labels
            .as_ref()
            .map_or(0, |t| t.read().len() as u64)
    }
}

#[uniffi::export]
pub fn capabilities() -> Capabilities {
    use sparkles::sparql::catalog;
    let mut features = Vec::new();
    if cfg!(feature = "text") {
        features.push("text".to_string());
    }
    if cfg!(feature = "geo") {
        features.push("geo".to_string());
    }
    for (enabled, name) in [
        (cfg!(feature = "reasoning"), "reasoning"),
        (cfg!(feature = "shacl"), "shacl"),
        (cfg!(feature = "shex"), "shex"),
        (cfg!(feature = "backup"), "backup"),
    ] {
        if enabled {
            features.push(name.into());
        }
    }
    Capabilities {
        functions: catalog::extension_functions(),
        aggregates: catalog::extension_aggregates(),
        property_functions: catalog::property_functions(),
        features,
    }
}

/// A read transaction: a pinned snapshot. Its calls run on the caller's thread.
#[derive(uniffi::Object)]
pub struct FfiReadTxn {
    ds: Arc<Inner>,
    snap: Arc<Snapshot>,
}

impl FfiReadTxn {
    fn reader<'a>(&'a self, labels: &'a Labels) -> SnapReader<'a> {
        SnapReader {
            snap: &self.snap,
            labels,
            term_cache: self.ds.term_cache(),
        }
    }
}

#[uniffi::export]
impl FfiReadTxn {
    /// The commit the snapshot shows.
    pub fn commit_seq(&self) -> u64 {
        self.snap.commit
    }

    pub fn find(&self, pattern: Vec<u8>, first_rows: u32) -> FfiResult<FindResult> {
        let labels = self.ds.labels();
        self.reader(&labels).find(&pattern, first_rows)
    }

    pub fn count(&self, pattern: Vec<u8>) -> FfiResult<u64> {
        let labels = self.ds.labels();
        self.reader(&labels).count(&pattern)
    }

    pub fn contains(&self, quad: Vec<u8>) -> FfiResult<bool> {
        let labels = self.ds.labels();
        self.reader(&labels).contains(&quad)
    }

    pub fn graph_names(&self) -> FfiResult<Vec<u8>> {
        let labels = self.ds.labels();
        self.reader(&labels).graph_names()
    }

    pub fn prepare_query(&self, text: String, options: QueryOpts) -> FfiResult<Arc<FfiQuery>> {
        Ok(Arc::new(FfiQuery::prepare(
            self.snap.clone(),
            text,
            &options,
            &self.ds.ds,
            self.ds.labels(),
            self.ds.term_cache(),
        )?))
    }
}

/// A write transaction. A worker thread holds the writer lock and applies the batches
/// sent to it; reads run on the caller's thread over a view of the transaction's state,
/// taken once after each change.
#[derive(uniffi::Object)]
pub struct FfiWriteTxn {
    ds: Arc<Inner>,
    base: u64,
    worker: Mutex<Option<TxnWorker>>,
    view: Mutex<Option<Arc<Snapshot>>>,
    /// the labels this transaction's writes used, merged into the dataset's at commit
    pending: Arc<RwLock<LabelTable>>,
    record_labels: bool,
    /// set by a failure that may have left part of its changes behind
    aborted: Mutex<Option<String>>,
}

fn ended() -> FfiError {
    FfiError::new(
        ErrorKind::TransactionEnded,
        "the write transaction has ended",
    )
}

/// `(insert, quad)` operations, and the blank node labels they use that name no stored
/// node yet.
type Ops = Vec<(bool, Quad)>;

impl FfiWriteTxn {
    fn labels(&self) -> Labels {
        Labels {
            dataset: self.ds.labels.clone(),
            txn: Some(self.pending.clone()),
        }
    }

    fn check_open(&self) -> FfiResult<()> {
        if let Some(why) = self.aborted.lock().as_ref() {
            return Err(FfiError::new(
                ErrorKind::TransactionEnded,
                format!("the transaction was aborted by an earlier failure: {why}"),
            ));
        }
        Ok(())
    }

    /// Run `f` on the worker; a change drops the cached view.
    fn run<R: Send + 'static>(
        &self,
        change: bool,
        f: impl FnOnce(&mut sparkles::Transaction<'_>) -> sparkles::Result<R> + Send + 'static,
    ) -> FfiResult<R> {
        self.check_open()?;
        let w = self.worker.lock();
        let w = w.as_ref().ok_or_else(ended)?;
        if change {
            *self.view.lock() = None;
        }
        Ok(w.run(f)?)
    }

    fn view(&self) -> FfiResult<Arc<Snapshot>> {
        if let Some(v) = self.view.lock().as_ref() {
            return Ok(v.clone());
        }
        let v = self.run(false, |tx| Ok(tx.snapshot()))?;
        *self.view.lock() = Some(v.clone());
        Ok(v)
    }

    fn with_reader<R>(&self, f: impl FnOnce(&SnapReader<'_>) -> FfiResult<R>) -> FfiResult<R> {
        let snap = self.view()?;
        let labels = self.labels();
        f(&SnapReader {
            snap: &snap,
            labels: &labels,
            term_cache: self.ds.term_cache(),
        })
    }

    fn decode_ops(&self, batch: &[u8]) -> FfiResult<Ops> {
        self.labels().with(|resolve, _| {
            let mut r = Reader::new(batch, resolve);
            let mut ops = Vec::new();
            while !r.at_end() {
                let insert = match r.byte()? {
                    encode::OP_ADD => true,
                    encode::OP_DELETE => false,
                    op => {
                        return Err(FfiError::new(
                            ErrorKind::Malformed,
                            format!("operation {op}"),
                        ));
                    }
                };
                let g = r.item()?;
                let s = r.item()?;
                let p = r.item()?;
                let o = r.item()?;
                let graph = match g {
                    Item::None | Item::DefaultGraph => GraphName::DefaultGraph,
                    Item::Term(Term::NamedNode(n)) if n.as_str() == encode::DEFAULT_GRAPH_IRI => {
                        GraphName::DefaultGraph
                    }
                    Item::UnionGraph => return Err(read::not_writable("cannot change")),
                    Item::Term(Term::NamedNode(n)) if n.as_str() == encode::UNION_GRAPH_IRI => {
                        return Err(read::not_writable("cannot change"));
                    }
                    Item::Term(Term::NamedNode(n)) => GraphName::NamedNode(n),
                    Item::Term(Term::BlankNode(b)) => GraphName::BlankNode(b),
                    Item::Term(_) => return Err(invalid("a graph name is an IRI or a blank node")),
                };
                let subject = match s {
                    Item::Term(Term::NamedNode(n)) => NamedOrBlankNode::NamedNode(n),
                    Item::Term(Term::BlankNode(b)) => NamedOrBlankNode::BlankNode(b),
                    Item::Term(Term::Triple(_)) | Item::Term(Term::Literal(_)) if !insert => {
                        continue;
                    }
                    _ => return Err(invalid("a subject is an IRI or a blank node")),
                };
                let predicate = match p {
                    Item::Term(Term::NamedNode(n)) => n,
                    Item::Term(_) if !insert => continue,
                    _ => return Err(invalid("a predicate is an IRI")),
                };
                let Item::Term(object) = o else {
                    return Err(invalid("a quad to write has an object"));
                };
                ops.push((insert, Quad::new(subject, predicate, object, graph)));
            }
            Ok(ops)
        })
    }
}

fn invalid(m: &str) -> FfiError {
    FfiError::new(ErrorKind::Invalid, m)
}

/// Each blank node label of a quad, inside triple terms too.
fn for_each_label(q: &Quad, f: &mut impl FnMut(&str)) {
    fn term(t: &Term, f: &mut impl FnMut(&str)) {
        match t {
            Term::BlankNode(b) => f(b.as_str()),
            Term::Triple(tr) => {
                if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                    f(b.as_str());
                }
                term(&tr.object, f);
            }
            _ => {}
        }
    }
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        f(b.as_str());
    }
    term(&q.object, f);
    if let GraphName::BlankNode(b) = &q.graph_name {
        f(b.as_str());
    }
}

#[uniffi::export]
impl FfiWriteTxn {
    /// The commit the transaction started from.
    pub fn base_seq(&self) -> u64 {
        self.base
    }

    /// Apply a batch of operations in order: a byte (1 add, 2 delete) and four terms
    /// (graph, subject, predicate, object) each. A failure aborts the transaction.
    pub fn apply(&self, batch: Vec<u8>) -> FfiResult<ApplyStats> {
        let ops = self.decode_ops(&batch)?;
        // a bulk load's labels stay in the worker's transaction, which maps them
        let record = self.record_labels;
        let r = self.run(true, move |tx| {
            let (mut ins, mut del) = (0u64, 0u64);
            let mut made: Vec<(String, Id)> = Vec::new();
            for (insert, q) in &ops {
                if *insert {
                    ins += tx.insert_linked(q.as_ref())? as u64;
                    if !record {
                        continue;
                    }
                    for_each_label(q, &mut |l| {
                        if let Some(b) = tx.blank_node(l)
                            && b.as_str() != l
                            && let Some(id) = sparkles::store::parse_bnode_label(b.as_str())
                        {
                            made.push((l.to_string(), id));
                        }
                    });
                } else {
                    del += tx.remove(q.as_ref())? as u64;
                }
            }
            Ok((ins, del, made))
        });
        match r {
            Ok((inserted, deleted, made)) => {
                if !made.is_empty() {
                    let mut p = self.pending.write();
                    for (l, id) in made {
                        p.insert(&l, id);
                    }
                }
                Ok(ApplyStats { inserted, deleted })
            }
            Err(e) => {
                if e.kind() != ErrorKind::TransactionEnded {
                    *self.aborted.lock() = Some(e.to_string());
                }
                Err(e)
            }
        }
    }

    /// Remove every quad of a pattern without sending the matches back.
    pub fn remove_matching(&self, pattern: Vec<u8>) -> FfiResult<u64> {
        let p = self
            .labels()
            .with(|resolve, _| read::decode_pattern(&pattern, resolve))?;
        let Some(p) = p else { return Ok(0) };
        self.run(true, move |tx| tx.remove_matching(&p))
    }

    pub fn find(&self, pattern: Vec<u8>, first_rows: u32) -> FfiResult<FindResult> {
        self.with_reader(|r| r.find(&pattern, first_rows))
    }

    pub fn count(&self, pattern: Vec<u8>) -> FfiResult<u64> {
        self.with_reader(|r| r.count(&pattern))
    }

    pub fn contains(&self, quad: Vec<u8>) -> FfiResult<bool> {
        self.with_reader(|r| r.contains(&quad))
    }

    pub fn graph_names(&self) -> FfiResult<Vec<u8>> {
        self.with_reader(|r| r.graph_names())
    }

    /// A query that sees the transaction's changes.
    pub fn prepare_query(&self, text: String, options: QueryOpts) -> FfiResult<Arc<FfiQuery>> {
        let snap = self.view()?;
        let mut query = FfiQuery::prepare(
            snap,
            text,
            &options,
            &self.ds.ds,
            self.labels(),
            self.ds.term_cache(),
        )?;
        query.forbid_cursor();
        Ok(Arc::new(query))
    }

    /// Run a SPARQL Update request in the transaction. A syntax error changes nothing;
    /// any other failure aborts the transaction, because the request may have stopped
    /// between operations.
    pub fn update(&self, text: String, options: QueryOpts) -> FfiResult<UpdateStats> {
        let opts = options.to_options(&self.labels(), Default::default())?;
        let r = self.run(true, move |tx| tx.update_with(&text, &opts));
        match r {
            Ok(s) => Ok(UpdateStats {
                inserted: s.inserted,
                deleted: s.deleted,
                operations: s.operations as u64,
                total_ms: s.timing.total_ms,
            }),
            Err(e) => {
                if !matches!(
                    e.kind(),
                    ErrorKind::SparqlSyntax | ErrorKind::TransactionEnded
                ) {
                    *self.aborted.lock() = Some(e.to_string());
                }
                Err(e)
            }
        }
    }

    /// Commit. The labels the transaction's writes used enter the dataset's table.
    pub fn commit(&self) -> FfiResult<Receipt> {
        let worker = self.worker.lock().take().ok_or_else(ended)?;
        *self.view.lock() = None;
        if let Some(why) = self.aborted.lock().clone() {
            worker.abort();
            return Err(FfiError::new(
                ErrorKind::TransactionEnded,
                format!(
                    "the transaction was aborted by an earlier failure and has been rolled back: {why}"
                ),
            ));
        }
        let receipt = worker.commit()?;
        if self.record_labels
            && let Some(t) = &self.ds.labels
        {
            let p = self.pending.read();
            if !p.is_empty() {
                t.write().merge(&p);
            }
        }
        Ok((&receipt).into())
    }

    /// Discard the transaction's changes and release the writer lock.
    pub fn abort(&self) {
        *self.view.lock() = None;
        if let Some(w) = self.worker.lock().take() {
            w.abort();
        }
    }

    /// Whether the transaction can still commit.
    pub fn is_open(&self) -> bool {
        self.aborted.lock().is_none() && self.worker.lock().as_ref().is_some_and(|w| w.is_open())
    }
}

#[cfg(test)]
mod tests;
