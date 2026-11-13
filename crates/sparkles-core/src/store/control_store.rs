//! The store side of the control keyspace (spec C26 §2.1 and §7): opening and
//! recovering it, enabling it, staging control records in a write transaction, the
//! control-only write path, checkpoints and the uncertainty guard.
//!
//! The ordering argument is C26 §7.1. Every frame is written under the writer lock and
//! is durable before the lock is released. In a store whose keyspace is enabled the
//! writer is marked poisoned before the first byte of a serial transaction, a
//! control-only frame or a generation switch reaches a file, and the mark is cleared
//! only after the head, the log's length, the snapshot and the control state are
//! published. An error or an unwind in between therefore leaves the writer poisoned,
//! and a poisoned writer neither writes another frame nor trims a log at close.

use super::*;
use crate::control::{self, ControlState, Frame, Op};

/// The owner of a part of the control keyspace, and so the prefix of its keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlOwner {
    Jobs,
}

impl ControlOwner {
    pub fn prefix(self) -> &'static [u8] {
        match self {
            ControlOwner::Jobs => b"jobs/",
        }
    }
}

/// The capability to stage control records of one owner in a write transaction
/// ([`WriteTxn::set_control`]). The core grants it to a typed owner API such as the jobs
/// handle, never to a query or an update.
#[derive(Clone, Copy, Debug)]
pub struct ControlWrite {
    owner: ControlOwner,
}

/// What a transaction's frame will be stamped with ([`WriteTxn::control_stamp`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlStamp {
    /// the control sequence number of the frame
    pub seq: u64,
    /// the business head the transaction starts from
    pub head: u64,
    /// the commit the transaction makes, when its quads changed so far
    pub commit: Option<u64>,
    pub time_ms: i64,
}

/// The open control log of the writer.
pub(crate) struct ControlLog {
    file: File,
    path: PathBuf,
    /// the bytes of the header and the complete records
    len: u64,
    /// the bytes of the last checkpoint, which the log must outgrow before it is
    /// replaced by a new one
    checkpoint_bytes: u64,
    /// the frame the last checkpoint holds the state after
    checkpoint_seq: u64,
}

/// What an open recovered.
pub(crate) struct Opened {
    pub(crate) state: ControlState,
    log_good: u64,
    log_torn: bool,
    log_missing: bool,
    checkpoint_bytes: u64,
    checkpoint_seq: u64,
    /// the checkpoint carried an origin entry that a recovered frame removed, so it
    /// must be written again before any transition (spec C26 §7.3)
    origin_consumed: bool,
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("control keyspace: {what}"))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Whether the dataset at `root` records that it needs a control keyspace: a
/// `jobs.json`, a jobs entry in the managed-graph registry as its file records it,
/// whether or not the registry can be enforced, or a write-ahead log of `wal_version` 3
/// or later. A registry file that does not read records nothing here, and its own
/// refusal stands.
pub(crate) fn jobs_configured(root: &Path, wal_version: u32) -> bool {
    root.join(crate::jobs::FILE).exists()
        || wal_version >= crate::formats::WAL_CONTROL_VERSION
        || std::fs::read(root.join(crate::managed::FILE))
            .ok()
            .and_then(|bytes| crate::managed::recorded_owners(&bytes).ok())
            .is_some_and(|owners| owners.contains(&crate::managed::OwnerKind::Jobs))
}

/// Recover the control keyspace of the store at `root` from its checkpoint, the frames
/// `wal` of the write-ahead log transactions the replay keeps and `control.log`, with
/// `head` the recovered business head (spec C26 §7.2). Nothing is written. `None` for a
/// store without a keyspace.
pub(crate) fn recover_at_open(
    root: &Path,
    wal: Vec<control::WalFrame>,
    head: u64,
    configured: bool,
) -> Result<Option<Opened>> {
    let bin = read_optional(&root.join(control::CHECKPOINT_FILE))?;
    let log = read_optional(&root.join(control::LOG_FILE))?;
    let Some(bin) = bin else {
        if log.is_some() {
            return Err(corrupt(format!(
                "{} exists without {}",
                control::LOG_FILE,
                control::CHECKPOINT_FILE
            )));
        }
        if configured {
            return Err(corrupt(format!(
                "jobs are configured but {} is missing",
                control::CHECKPOINT_FILE
            )));
        }
        if !wal.is_empty() {
            return Err(corrupt(format!(
                "the write-ahead log carries control frames but {} is missing",
                control::CHECKPOINT_FILE
            )));
        }
        return Ok(None);
    };
    let checkpoint = control::decode_checkpoint(&bin)?;
    let origin = checkpoint.origin()?;
    if let Some(o) = &origin
        && (o.kind != "capture" || o.commit != checkpoint.follows)
    {
        return Err(corrupt(
            "the checkpoint's origin entry does not name its commit",
        ));
    }
    let (frames, log_good, log_torn, log_missing) = match &log {
        Some(bytes) => {
            let r = control::read_log(bytes)?;
            (r.frames, r.good, r.torn, false)
        }
        None => {
            // An interrupted enablement, or a restored checkpoint before its first
            // writable open. Anywhere else the log was lost with acknowledged frames.
            let initial = checkpoint.seq == 0 && checkpoint.is_empty() && !configured;
            if !initial && origin.is_none() {
                return Err(corrupt(format!(
                    "{} is missing beside a checkpoint at frame {}",
                    control::LOG_FILE,
                    checkpoint.seq
                )));
            }
            (Vec::new(), control::LOG_HEADER_BYTES, false, true)
        }
    };
    let checkpoint_seq = checkpoint.seq;
    let state = control::recover(checkpoint, wal, &frames, head)?;
    let origin_consumed = origin.is_some() && state.get(control::ORIGIN_KEY).is_none();
    Ok(Some(Opened {
        state,
        log_good,
        log_torn,
        log_missing,
        checkpoint_bytes: bin.len() as u64,
        checkpoint_seq,
        origin_consumed,
    }))
}

/// Make what [`recover_at_open`] accepted durable before anything is written after it:
/// cut a torn tail, create a missing log, synchronize the log and the root directory,
/// and write the checkpoint again when the checkpoint's origin was consumed. Called
/// after the write-ahead log was truncated and synchronized.
pub(super) fn settle_at_open(root: &Path, opened: &Opened) -> Result<ControlLog> {
    let path = root.join(control::LOG_FILE);
    let mut checkpoint_bytes = opened.checkpoint_bytes;
    let mut checkpoint_seq = opened.checkpoint_seq;
    let len = if opened.log_missing || opened.origin_consumed {
        if opened.origin_consumed {
            let bytes = control::encode_checkpoint(&opened.state);
            write_atomic(&root.join(control::CHECKPOINT_FILE), &bytes)?;
            sync_dir(root)?;
            checkpoint_bytes = bytes.len() as u64;
            checkpoint_seq = opened.state.seq;
        }
        write_atomic(&path, &control::log_header())?;
        control::LOG_HEADER_BYTES
    } else {
        if opened.log_torn {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(opened.log_good)?;
        }
        opened.log_good
    };
    let file = OpenOptions::new().read(true).write(true).open(&path)?;
    // bytes recovered from the page cache of a process that stopped are durable before
    // anything is written after them (spec C26 §7.2)
    file.sync_data()?;
    sync_dir(root)?;
    Ok(ControlLog {
        file,
        path,
        len,
        checkpoint_bytes,
        checkpoint_seq,
    })
}

/// The checkpoint a capture renders from the control state `state` taken under the
/// writer lock: with an origin entry that names the capture, the source dataset and the
/// captured commit, which is also the checkpoint's business head (spec C26 §7.3).
pub(crate) fn capture_checkpoint(
    state: &ControlState,
    dataset: uuid::Uuid,
    commit: u64,
    label: &str,
) -> Result<Vec<u8>> {
    let origin = control::Origin {
        kind: "capture".into(),
        dataset,
        commit,
        label: label.into(),
        time_ms: crate::commit::now_ms(),
    };
    let value = serde_json::to_vec(&origin).map_err(|e| corrupt(e))?;
    let mut rendered = state.with_entry(control::ORIGIN_KEY, Some(value.into()));
    rendered.follows = commit;
    Ok(control::encode_checkpoint(&rendered))
}

/// The staged control changes of a write transaction.
pub(crate) struct ControlTxn {
    owner: ControlOwner,
    base: Arc<ControlState>,
    changes: BTreeMap<Box<[u8]>, Option<Arc<[u8]>>>,
}

impl Store {
    /// The published control state, or `None` for a store without a keyspace. A reader
    /// that also needs a snapshot loads this first (spec C26 §7.1, property 5).
    pub fn control_state(&self) -> Option<Arc<ControlState>> {
        self.control.load_full()
    }

    /// Whether the store's control keyspace is enabled.
    pub fn control_enabled(&self) -> bool {
        self.control.load().is_some()
    }

    /// The capability to stage `owner`'s control records. Fails when the keyspace is not
    /// enabled.
    pub fn control_writer(&self, owner: ControlOwner) -> Result<ControlWrite> {
        if !self.control_enabled() {
            return Err(Error::invalid(
                "the dataset has no control keyspace; enable it first",
            ));
        }
        Ok(ControlWrite { owner })
    }

    /// Enable the control keyspace of the main store (spec C26 §7): an empty checkpoint,
    /// then an empty log, then the current write-ahead log's header at version 3, each
    /// durable before the next. The caller raised the dataset reader to 9 first. Enabling
    /// an enabled keyspace finishes an interrupted enablement and changes nothing else.
    pub fn enable_control(&self) -> Result<()> {
        if self.branching.ident.is_some() {
            return Err(Error::invalid("a branch has no control keyspace"));
        }
        // the reader that knows the keyspace is durable before its first byte
        self.require_dataset_reader(crate::jobs::READER)?;
        let mut w = self.lock_writer(&Default::default())?;
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        if let Some(root) = self.root.clone() {
            let bin = root.join(control::CHECKPOINT_FILE);
            if !bin.exists() {
                write_atomic(&bin, &control::encode_checkpoint(&ControlState::default()))?;
                sync_dir(&root)?;
                self.failpoint("control-enable-checkpoint");
            }
            if w.control_log.is_none() {
                let path = root.join(control::LOG_FILE);
                if !path.exists() {
                    write_atomic(&path, &control::log_header())?;
                    sync_dir(&root)?;
                    self.failpoint("control-enable-log");
                }
                let file = OpenOptions::new().read(true).write(true).open(&path)?;
                let bytes = std::fs::read(&path)?;
                let r = control::read_log(&bytes)?;
                if !r.frames.is_empty() || r.torn {
                    return Err(corrupt(format!(
                        "{} holds frames of a keyspace this store did not open",
                        control::LOG_FILE
                    )));
                }
                w.control_log = Some(ControlLog {
                    file,
                    path,
                    len: r.good,
                    checkpoint_bytes: 0,
                    checkpoint_seq: 0,
                });
            }
        }
        if self.control.load().is_none() {
            self.control.store(Some(Arc::new(ControlState::default())));
        }
        self.upgrade_wal_header(&mut w)?;
        Ok(())
    }

    /// Upgrade the current log's header to version 3 through the writer's own handle
    /// (spec C26 §7): the group is drained (the caller holds the writer through
    /// `lock_writer`), the buffered writer flushed, the direct writer's cached block
    /// dropped, the header written and synchronized, and the writer's length and
    /// allocation kept. A failure poisons the writer.
    fn upgrade_wal_header(&self, w: &mut WriterState) -> Result<()> {
        if w.wal.is_none() {
            return Ok(());
        }
        let Some(path) = self
            .snapshot()
            .generation
            .dir
            .as_deref()
            .map(|d| d.join("wal.log"))
        else {
            return Ok(());
        };
        // the writer's handle is opened for writing only, and under the writer lock the
        // current log's header changes through no other handle
        if wal::file_version(&path)? == crate::formats::WAL_CONTROL_VERSION {
            return Ok(());
        }
        self.drain_group(w, None)?;
        let tail = w.tail.clone();
        let mut io = tail.lock();
        w.poisoned = true;
        let r = (|| -> Result<()> {
            w.wal.as_mut().expect("checked").flush()?;
            if let wal::Direct::Open(d) = &mut io.direct {
                d.invalidate();
            }
            let file = w.wal.as_ref().expect("checked").get_ref();
            wal::write_all_at(
                file,
                &wal::header_record_of(crate::formats::WAL_CONTROL_VERSION),
                0,
            )?;
            file.sync_data()?;
            Ok(())
        })();
        tail.release(&io);
        drop(io);
        r?;
        self.failpoint("control-header-upgraded");
        w.poisoned = false;
        Ok(())
    }

    /// Write a checkpoint of the published control state and replace the log by an
    /// empty one (spec C26 §7.1), when a frame was written since the last checkpoint.
    /// Called under the writer lock, before a generation switch and when the log grows.
    pub(super) fn checkpoint_control(&self, w: &mut WriterState) -> Result<()> {
        let (Some(root), Some(state)) = (self.root.as_deref(), self.control.load_full()) else {
            return Ok(());
        };
        let Some(log) = w.control_log.as_mut() else {
            return Ok(());
        };
        if state.seq == log.checkpoint_seq && log.len == control::LOG_HEADER_BYTES {
            // no frame since the last checkpoint, in either log
            return Ok(());
        }
        let bytes = control::encode_checkpoint(&state);
        write_atomic(&root.join(control::CHECKPOINT_FILE), &bytes)?;
        sync_dir(root)?;
        self.failpoint("control-checkpoint-written");
        write_atomic(&log.path, &control::log_header())?;
        sync_dir(root)?;
        log.file = OpenOptions::new().read(true).write(true).open(&log.path)?;
        log.len = control::LOG_HEADER_BYTES;
        log.checkpoint_bytes = bytes.len() as u64;
        log.checkpoint_seq = state.seq;
        self.failpoint("control-log-replaced");
        Ok(())
    }

    /// Append the control-only `frame` to the log and publish `state` (spec C26 §7.1):
    /// the writer's head and generation must be those of the published snapshot, the
    /// writer is poisoned while the frame is uncertain, and the log is synchronized
    /// before the state is published.
    fn append_control(
        &self,
        w: &mut WriterState,
        frame: &Frame,
        state: ControlState,
    ) -> Result<()> {
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        let snap = self.snapshot();
        if snap.commit != w.head.seq || w.staged.is_some() {
            return Err(Error::Poisoned);
        }
        let bytes = frame.encode()?;
        if let Some(log) = w.control_log.as_mut() {
            let record = control::log_record(&bytes);
            self.check_disk(record.len() as u64)?;
            w.poisoned = true;
            self.failpoint("control-before-append");
            wal::write_all_at(&log.file, &record, log.len)?;
            self.failpoint("control-appended");
            log.file.sync_data()?;
            log.len += record.len() as u64;
            self.failpoint("control-synced");
        }
        self.control.store(Some(Arc::new(state)));
        self.failpoint("control-published");
        w.poisoned = false;
        if let Some(log) = w.control_log.as_ref()
            && log.len > control::LOG_CHECKPOINT_BYTES
            && log.len > log.checkpoint_bytes
        {
            // The frame is durable and published. A failed checkpoint may have
            // replaced one file and not the other, which an open sorts out, so it
            // poisons the writer instead of failing the acknowledged frame.
            w.poisoned = true;
            if let Err(e) = self.checkpoint_control(w) {
                tracing::error!(target: "sparkles::store", error = %e, "control checkpoint failed");
                return Ok(());
            }
            w.poisoned = false;
        }
        Ok(())
    }
}

impl Store {
    /// Make the next control-only frame return `refusal()` before it writes anything, as
    /// a refusal under the writer would (tests only).
    #[cfg(any(test, feature = "failpoints"))]
    pub fn refuse_next_control_frame(&self, refusal: fn() -> Error) {
        *self.control_refusal.lock() = Some(refusal);
    }
}

impl WriteTxn<'_> {
    /// Stage control records of `cap`'s owner in this transaction. The transaction then
    /// commits through the serial path, never in a group or as a bulk rebuild, and a dry
    /// run is refused (spec C26 §7.1).
    pub fn set_control(&mut self, cap: ControlWrite) -> Result<()> {
        if self.opts.dry_run.is_some() {
            return Err(Error::invalid("a dry run cannot stage control records"));
        }
        if !self.bulk.is_empty() {
            return Err(Error::invalid(
                "a bulk transaction cannot stage control records",
            ));
        }
        let base = self.store.control_state().ok_or_else(|| {
            Error::invalid("the dataset has no control keyspace; enable it first")
        })?;
        self.control = Some(ControlTxn {
            owner: cap.owner,
            base,
            changes: BTreeMap::new(),
        });
        Ok(())
    }

    fn control_txn(&self) -> Result<&ControlTxn> {
        self.control
            .as_ref()
            .ok_or_else(|| Error::invalid("the transaction stages no control records"))
    }

    fn owned(&self, key: &[u8]) -> Result<()> {
        let prefix = self.control_txn()?.owner.prefix();
        if !key.starts_with(prefix) {
            return Err(Error::invalid(format!(
                "control key outside the owner's prefix {}",
                String::from_utf8_lossy(prefix)
            )));
        }
        Ok(())
    }

    /// The control state the transaction started from: the published state when the
    /// writer was taken.
    pub fn control_base(&self) -> Result<Arc<ControlState>> {
        Ok(self.control_txn()?.base.clone())
    }

    /// The bytes of the keys and values of the control state as this transaction would
    /// leave it, which `maxControlBytes` bounds ([`ControlState::bytes`]).
    pub fn control_bytes(&self) -> Result<u64> {
        let c = self.control_txn()?;
        let mut bytes = c.base.bytes();
        for (k, v) in &c.changes {
            if let Some(old) = c.base.get(k) {
                bytes -= (k.len() + old.len()) as u64;
            }
            if let Some(v) = v {
                bytes += (k.len() + v.len()) as u64;
            }
        }
        Ok(bytes)
    }

    /// The value of `key` as this transaction sees it.
    pub fn control_get(&self, key: &[u8]) -> Result<Option<Arc<[u8]>>> {
        let c = self.control_txn()?;
        Ok(match c.changes.get(key) {
            Some(v) => v.clone(),
            None => c.base.get(key).cloned(),
        })
    }

    /// Up to `limit` entries whose keys start with `prefix`, as this transaction sees
    /// them, in key order, and whether more exist.
    pub fn control_prefix(
        &self,
        prefix: &[u8],
        limit: usize,
    ) -> Result<(Vec<(Box<[u8]>, Arc<[u8]>)>, bool)> {
        let c = self.control_txn()?;
        let mut base = c.base.prefix(prefix).peekable();
        let start: Box<[u8]> = prefix.into();
        let mut staged = c
            .changes
            .range(start..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .peekable();
        let mut out = Vec::new();
        loop {
            let next = match (base.peek(), staged.peek()) {
                (None, None) => break,
                (Some(_), None) => base.next().map(|(k, v)| (k.clone(), Some(v.clone()))),
                (None, Some(_)) => staged.next().map(|(k, v)| (k.clone(), v.clone())),
                (Some((bk, _)), Some((sk, _))) => {
                    if bk < sk {
                        base.next().map(|(k, v)| (k.clone(), Some(v.clone())))
                    } else {
                        if bk == sk {
                            base.next();
                        }
                        staged.next().map(|(k, v)| (k.clone(), v.clone()))
                    }
                }
            };
            if let Some((k, Some(v))) = next {
                if out.len() == limit {
                    return Ok((out, true));
                }
                out.push((k, v));
            }
        }
        Ok((out, false))
    }

    /// Stage `value` under `key`, which must start with the owner's prefix.
    pub fn control_put(&mut self, key: &[u8], value: Arc<[u8]>) -> Result<()> {
        self.owned(key)?;
        control::check_entry(key, Some(&value))?;
        self.control
            .as_mut()
            .expect("owned checked")
            .changes
            .insert(key.into(), Some(value));
        Ok(())
    }

    /// Stage the removal of `key`, which must start with the owner's prefix.
    pub fn control_delete(&mut self, key: &[u8]) -> Result<()> {
        self.owned(key)?;
        self.control
            .as_mut()
            .expect("owned checked")
            .changes
            .insert(key.into(), None);
        Ok(())
    }

    /// The origin entry of a restored checkpoint, as this transaction sees it.
    pub fn control_origin(&self) -> Result<Option<control::Origin>> {
        match self.control_get(control::ORIGIN_KEY)? {
            None => Ok(None),
            Some(v) => serde_json::from_slice(&v)
                .map(Some)
                .map_err(|e| corrupt(format!("origin entry: {e}"))),
        }
    }

    /// Stage the removal of the origin entry. The store writes a checkpoint right after
    /// the frame that removes it, before the writer is released (spec C26 §7.3).
    pub fn control_consume_origin(&mut self) -> Result<()> {
        self.control_txn()?;
        self.control
            .as_mut()
            .expect("checked")
            .changes
            .insert(control::ORIGIN_KEY.into(), None);
        Ok(())
    }

    /// The control sequence number this transaction's frame will take, the business head
    /// it starts from, the commit it makes when its quads changed so far, and its time,
    /// which the commit takes too.
    pub fn control_stamp(&mut self) -> Result<ControlStamp> {
        let seq = self.control_txn()?.base.seq + 1;
        let head = self.guard.head;
        let time_ms = *self
            .stamp
            .get_or_insert_with(|| self.store.commit_time(&head));
        let changed = self.net_ins > 0 || self.net_del > 0 || self.force;
        Ok(ControlStamp {
            seq,
            head: head.seq,
            commit: changed.then_some(head.seq + 1),
            time_ms,
        })
    }

    /// About the bytes the transaction's frame adds to the write-ahead log, for the
    /// storage checks.
    pub(super) fn control_wal_bytes(&self) -> u64 {
        let Some(c) = self.control.as_ref() else {
            return 0;
        };
        let frame: usize = 64
            + c.changes
                .iter()
                .map(|(k, v)| 16 + k.len() + v.as_ref().map_or(0, |v| v.len()))
                .sum::<usize>();
        (frame.div_ceil(control::CHUNK) * WAL_REC) as u64
    }

    /// Whether the transaction stages any control change.
    pub(super) fn control_staged(&self) -> bool {
        self.control.as_ref().is_some_and(|c| !c.changes.is_empty())
    }

    /// The transaction's frame, following commit `follows`, and the state it leads to.
    pub(super) fn control_frame(
        &self,
        follows: u64,
        time_ms: i64,
    ) -> Result<Option<(Frame, ControlState)>> {
        let Some(c) = self.control.as_ref().filter(|c| !c.changes.is_empty()) else {
            return Ok(None);
        };
        let published = self.store.control_state().ok_or(Error::Poisoned)?;
        if published.seq != c.base.seq {
            // another frame was written since this transaction began, which the writer
            // lock excludes
            return Err(Error::Poisoned);
        }
        let ops = c
            .changes
            .iter()
            .map(|(k, v)| match v {
                Some(v) => Op::Put(k.clone(), v.clone()),
                None => Op::Delete(k.clone()),
            })
            .collect();
        let frame = Frame {
            seq: c.base.seq + 1,
            follows,
            time_ms,
            ops,
        };
        let state = c.base.apply(&frame)?;
        Ok(Some((frame, state)))
    }

    /// Commit a transaction with control changes and no net change to the quads as a
    /// control-only frame (spec C26 §7.1). It is not a commit, and the receipt says so.
    pub(super) fn publish_control_only(&mut self) -> Result<Receipt> {
        #[cfg(any(test, feature = "failpoints"))]
        if let Some(refusal) = self.store.control_refusal.lock().take() {
            return Err(refusal());
        }
        let head = self.guard.head;
        let time_ms = self.stamp.unwrap_or_else(|| self.store.commit_time(&head));
        let (frame, state) = self
            .control_frame(head.seq, time_ms)?
            .expect("the caller checked control_staged");
        let consumes_origin = self
            .control
            .as_ref()
            .is_some_and(|c| c.base.get(control::ORIGIN_KEY).is_some())
            && state.get(control::ORIGIN_KEY).is_none();
        let w = &mut **self.guard;
        self.store.append_control(w, &frame, state)?;
        if consumes_origin {
            // the origin leaves the checkpoint before the writer is released, so a lost
            // log is never recreated empty after a transition was acknowledged
            w.poisoned = true;
            self.store.checkpoint_control(w)?;
            w.poisoned = false;
        }
        Ok(Receipt {
            dataset_id: self.store.owner_dataset_id(),
            committed: false,
            commit: head,
            validation: None,
            annotation: self.store.annotation(head.seq).unwrap_or_default(),
        })
    }
}
