//! Dataset administration on the facade. Kotlin handles retain and check their owner.
use crate::{ErrorKind, FfiDataset, FfiError, FfiReadTxn, FfiResult};
use sparkles::history::{At, SnapshotOptions};
use sparkles::task::{Cancel, Control, Progress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, uniffi::Record)]
pub struct OperationProgress {
    pub fraction: f32,
    pub message: String,
}

/// An independently cancellable operation; progress can be polled from another thread.
#[derive(uniffi::Object)]
pub struct FfiOperation {
    pub(crate) control: Control,
    progress: Arc<Mutex<OperationProgress>>,
}

#[uniffi::export]
impl FfiOperation {
    #[uniffi::constructor]
    pub fn new(timeout_ms: Option<u64>) -> Arc<Self> {
        let progress = Arc::new(Mutex::new(OperationProgress {
            fraction: 0.0,
            message: String::new(),
        }));
        let p = progress.clone();
        Arc::new(Self {
            control: Control {
                cancel: Cancel::new(),
                deadline: timeout_ms
                    .and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms))),
                progress: Progress::new(move |fraction, message| {
                    *p.lock().unwrap_or_else(|e| e.into_inner()) = OperationProgress {
                        fraction,
                        message: message.into(),
                    };
                }),
            },
            progress,
        })
    }
    pub fn cancel(&self) {
        self.control.cancel.cancel();
    }
    pub fn progress(&self) -> OperationProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SnapshotInfo {
    pub name: String,
    pub seq: u64,
    pub created_ms: i64,
    pub note: Option<String>,
    pub expires_ms: Option<i64>,
    pub reconstructable: bool,
    pub warm: bool,
}
impl From<sparkles::history::NamedSnapshot> for SnapshotInfo {
    fn from(s: sparkles::history::NamedSnapshot) -> Self {
        Self {
            name: s.name,
            seq: s.seq,
            created_ms: s.created_ms,
            note: s.note,
            expires_ms: s.expires_ms,
            reconstructable: s.reconstructable,
            warm: s.warm,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct DescribeSettings {
    pub mode: String,
    pub labels: bool,
    pub reifiers: bool,
    pub max_triples: Option<u64>,
    pub max_depth: Option<u32>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TextSettings {
    /// None indexes every predicate; an empty list indexes none.
    pub predicates: Option<Vec<String>>,
    pub max_text_bytes: u64,
    pub max_hits: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TextInfo {
    pub enabled: bool,
    pub state: String,
    pub docs: u64,
    pub seq: u64,
    pub store_seq: u64,
    pub disk_bytes: u64,
}
impl From<sparkles::text::TextStatus> for TextInfo {
    fn from(s: sparkles::text::TextStatus) -> Self {
        Self {
            enabled: s.enabled,
            state: s.state,
            docs: s.docs,
            seq: s.seq,
            store_seq: s.store_seq,
            disk_bytes: s.disk_bytes,
        }
    }
}

fn bad(s: impl ToString) -> FfiError {
    FfiError::new(ErrorKind::Invalid, s.to_string())
}
fn rdf_format(s: &str) -> FfiResult<oxrdfio::RdfFormat> {
    oxrdfio::RdfFormat::from_media_type(s)
        .or_else(|| oxrdfio::RdfFormat::from_extension(s))
        .ok_or_else(|| bad(format!("unknown RDF format {s}")))
}

#[uniffi::export]
impl FfiDataset {
    pub fn begin_read_at(&self, at: String) -> FfiResult<Arc<FfiReadTxn>> {
        let (snap, _) = self
            .inner
            .ds
            .store()
            .snapshot_at(&at.parse::<At>()?, &Default::default())?;
        Ok(Arc::new(FfiReadTxn {
            ds: self.inner.clone(),
            snap,
        }))
    }

    pub fn commits(
        &self,
        direction: String,
        cursor: Option<u64>,
        limit: u32,
    ) -> FfiResult<Vec<crate::CommitInfo>> {
        use sparkles::commit::CommitRange;
        let range = match (direction.as_str(), cursor) {
            ("latest", None) => CommitRange::Latest,
            ("before", Some(n)) => CommitRange::Before(n),
            ("after", Some(n)) => CommitRange::After(n),
            _ => return Err(bad("expected latest, before(cursor), or after(cursor)")),
        };
        Ok(self
            .inner
            .ds
            .history()
            .commits(range, limit as usize)
            .commits
            .iter()
            .map(Into::into)
            .collect())
    }

    pub fn snapshots_list(&self) -> Vec<SnapshotInfo> {
        self.inner
            .ds
            .snapshots()
            .list()
            .into_iter()
            .map(Into::into)
            .collect()
    }
    pub fn snapshots_get(&self, name: String) -> Option<SnapshotInfo> {
        self.inner.ds.snapshots().get(&name).map(Into::into)
    }
    pub fn snapshots_create(
        &self,
        name: String,
        at: String,
        note: Option<String>,
        expires_ms: Option<i64>,
        warm: bool,
    ) -> FfiResult<SnapshotInfo> {
        self.inner.check_writable()?;
        Ok(self
            .inner
            .ds
            .snapshots()
            .create(
                &name,
                &at.parse::<At>()?,
                &SnapshotOptions {
                    note,
                    expires_ms,
                    warm,
                },
            )?
            .0
            .into())
    }
    pub fn snapshots_delete(&self, name: String) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.snapshots().delete(&name)?)
    }

    pub fn describe_get(&self) -> DescribeSettings {
        let s = self.inner.ds.settings().describe().get();
        DescribeSettings {
            mode: s.mode.name().into(),
            labels: s.labels,
            reifiers: s.reifiers,
            max_triples: s.max_triples,
            max_depth: s.max_depth,
        }
    }
    pub fn describe_set(&self, s: DescribeSettings) -> FfiResult<()> {
        self.inner.check_writable()?;
        self.inner
            .ds
            .settings()
            .describe()
            .set(sparkles::sparql::describe::DescribeOptions {
                mode: sparkles::sparql::describe::DescribeMode::parse(&s.mode)?,
                labels: s.labels,
                reifiers: s.reifiers,
                max_triples: s.max_triples,
                max_depth: s.max_depth,
            })?;
        Ok(())
    }
    pub fn describe_reset(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.settings().describe().reset()?)
    }

    pub fn text_status(&self) -> Option<TextInfo> {
        self.inner.ds.indexes().text().status().map(Into::into)
    }
    pub fn text_enable(&self, s: TextSettings) -> FfiResult<TextInfo> {
        self.inner.check_writable()?;
        let cfg = sparkles::text::TextConfig {
            predicates: s.predicates.map_or(
                sparkles::text::PredicateSet::All,
                sparkles::text::PredicateSet::Only,
            ),
            max_text_bytes: usize::try_from(s.max_text_bytes).map_err(bad)?,
            max_hits: usize::try_from(s.max_hits).map_err(bad)?,
            ..Default::default()
        };
        Ok(self.inner.ds.indexes().text().enable(cfg)?.into())
    }
    pub fn text_disable(&self) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().text().disable()?)
    }
    pub fn text_rebuild(&self) -> FfiResult<TextInfo> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.indexes().text().rebuild()?.into())
    }

    pub fn compact(&self, operation: Arc<FfiOperation>) -> FfiResult<()> {
        self.inner.check_writable()?;
        self.inner
            .ds
            .compact_with(&Default::default(), &operation.control)?;
        Ok(())
    }
    pub fn clone_to(&self, path: String, operation: Arc<FfiOperation>) -> FfiResult<()> {
        operation.control.check()?;
        self.inner
            .ds
            .clone_to_with(path, &Default::default(), &operation.control)?;
        Ok(())
    }
    pub fn backup(&self, path: String) -> FfiResult<String> {
        Ok(self.inner.ds.backup(path)?.display().to_string())
    }
    pub fn dump_to_path(&self, path: String, format: String, at: Option<String>) -> FfiResult<u64> {
        let format = rdf_format(&format)?;
        let snap = match at {
            Some(at) => self.begin_read_at(at)?.snap.clone(),
            None => self.head(),
        };
        let file = std::fs::File::create(path).map_err(sparkles::Error::from)?;
        dump_snapshot(&snap, std::io::BufWriter::new(file), format)
    }
}

pub(crate) fn dump_snapshot(
    snap: &sparkles::store::Snapshot,
    w: impl std::io::Write,
    format: oxrdfio::RdfFormat,
) -> FfiResult<u64> {
    let mut out = oxrdfio::RdfSerializer::from_format(format).for_writer(w);
    let mut count = 0;
    snap.for_each_quad(|ids| {
        if !format.supports_datasets() && ids[3] != sparkles::id::Id::DEFAULT_GRAPH {
            return Ok(());
        }
        if let Some(q) = snap.quad_to_terms(ids) {
            if format.supports_datasets() {
                out.serialize_quad(&q)?;
            } else {
                out.serialize_triple(oxrdf::TripleRef::new(&q.subject, &q.predicate, &q.object))?;
            }
            count += 1;
        }
        Ok(())
    })?;
    out.finish().map_err(sparkles::Error::from)?;
    Ok(count)
}

#[uniffi::export]
impl FfiReadTxn {
    pub fn fork(&self) -> Arc<FfiReadTxn> {
        Arc::new(Self {
            ds: self.ds.clone(),
            snap: self.snap.clone(),
        })
    }
    pub fn dump(&self, format: String) -> FfiResult<Vec<u8>> {
        let mut bytes = Vec::new();
        dump_snapshot(&self.snap, &mut bytes, rdf_format(&format)?)?;
        Ok(bytes)
    }
}

#[derive(Clone)]
struct ChunkWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for ChunkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct DumpState {
    quads: sparkles::QuadIter,
    serializer: Option<oxrdfio::WriterQuadSerializer<ChunkWriter>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    datasets: bool,
}
/// Streaming serialization; byte chunks are copied once across the native boundary.
#[derive(uniffi::Object)]
pub struct FfiDump {
    state: Mutex<Option<DumpState>>,
}
impl FfiDump {
    fn new(snap: Arc<sparkles::store::Snapshot>, format: oxrdfio::RdfFormat) -> Arc<Self> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let serializer =
            oxrdfio::RdfSerializer::from_format(format).for_writer(ChunkWriter(bytes.clone()));
        Arc::new(Self {
            state: Mutex::new(Some(DumpState {
                quads: sparkles::embed::quads_in(snap, &sparkles::embed::QuadPattern::any()),
                serializer: Some(serializer),
                bytes,
                datasets: format.supports_datasets(),
            })),
        })
    }
}
#[uniffi::export]
impl FfiDump {
    pub fn next_chunk(
        &self,
        max_bytes: u32,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<crate::Batch> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = state.as_mut() else {
            return Ok(crate::Batch {
                batch: Vec::new(),
                done: true,
            });
        };
        operation.control.check()?;
        let limit = max_bytes.clamp(1, 1 << 20) as usize;
        let mut done = false;
        while s.bytes.lock().unwrap_or_else(|e| e.into_inner()).len() < limit {
            operation.control.check()?;
            let Some(q) = s.quads.next() else {
                if let Some(serializer) = s.serializer.take() {
                    serializer.finish().map_err(sparkles::Error::from)?;
                }
                done = true;
                break;
            };
            let q = q?;
            if !s.datasets && q.graph_name != oxrdf::GraphName::DefaultGraph {
                continue;
            }
            let serializer = s.serializer.as_mut().unwrap();
            if s.datasets {
                serializer.serialize_quad(&q)
            } else {
                serializer.serialize_triple(oxrdf::TripleRef::new(
                    &q.subject,
                    &q.predicate,
                    &q.object,
                ))
            }
            .map_err(sparkles::Error::from)?;
        }
        let bytes = std::mem::take(&mut *s.bytes.lock().unwrap_or_else(|e| e.into_inner()));
        if done {
            *state = None;
        }
        Ok(crate::Batch { batch: bytes, done })
    }
    pub fn release(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
#[uniffi::export]
impl FfiReadTxn {
    pub fn dump_cursor(&self, format: String) -> FfiResult<Arc<FfiDump>> {
        Ok(FfiDump::new(self.snap.clone(), rdf_format(&format)?))
    }
}
