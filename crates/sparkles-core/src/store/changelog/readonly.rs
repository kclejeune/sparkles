//! Exact-file, read-only history attachment for a cold retained origin.
//! It has no writer, recovery, retention or sidecar-index capability.
use super::*;
use crate::store::cold::{ColdBudget, Loaded};

pub(super) struct CapturedSegment {
    first: u64,
    pub(super) extent: u64,
    file: File,
}

pub(super) struct ReadOnlyLog {
    pub(super) segments: Vec<CapturedSegment>,
    /// A caller can narrow this bound, but cannot read a later branch state.
    pub(super) through: u64,
    /// Proven branch head bounds time-selection metadata. Row scans still stop
    /// at through, matching ordinary query time resolution followed by clipping.
    head: u64,
}

impl ReadOnlyLog {
    /// The caller holds the origin's opening and retention guards until capture
    /// finishes. Later reads use these descriptors, never reopen their paths.
    pub(super) fn capture(
        root: &Path,
        dataset: uuid::Uuid,
        through: u64,
        head: u64,
        budget: &mut ColdBudget<'_>,
    ) -> Result<Loaded<Self>> {
        budget.check()?;
        if through > head {
            return Err(Error::Corrupt(
                "history view is beyond its proven branch head".into(),
            ));
        }
        let start = budget.used();
        // Include directory iteration buffers and transient names/paths before
        // asking the filesystem for entries, rather than charging after collection.
        let scratch = (64 << 10) + root.as_os_str().len() as u64 * 4;
        budget.charge(scratch)?;
        let result: Result<Self> = (|| {
            let dir = root.join(CHANGES_DIR);
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let mut segments = Vec::new();
            for entry in entries.into_iter().flatten() {
                budget.key_step("history log segment", segments.len())?;
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some((stem, "log")) = name.split_once('.') else {
                    continue;
                };
                let Ok(first) = stem.parse::<u64>() else {
                    continue;
                };
                if first > head {
                    continue;
                }
                let mut file = File::open(entry.path())?;
                let extent = file.metadata()?.len();
                let mut header = [0; SEG_HEADER];
                if extent < SEG_HEADER as u64 {
                    return Err(Error::HistoryUnsupported(
                        "a retained origin has an incomplete history-log header".into(),
                    ));
                }
                file.read_exact(&mut header)?;
                budget.check()?;
                if &header[..8] != SEG_MAGIC {
                    return Err(Error::Corrupt(
                        "retained history log has no segment magic".into(),
                    ));
                }
                crate::formats::CHANGE_LOG.check(
                    "retained history log",
                    u32::from_le_bytes(header[8..12].try_into().unwrap()),
                    None,
                )?;
                if header[16..32] != *dataset.as_bytes() {
                    return Err(Error::Corrupt(
                        "retained history log has another dataset identity".into(),
                    ));
                }
                let need = segments.len() + 1;
                budget.reserve(&mut segments, need)?;
                segments.push(CapturedSegment {
                    first,
                    extent,
                    file,
                });
            }
            budget.sort_by(&mut segments, |a, b| a.first.cmp(&b.first))?;
            for pair in segments.windows(2) {
                budget.check()?;
                if pair[0].first == pair[1].first {
                    return Err(Error::Corrupt(
                        "retained history log has duplicate segment identities".into(),
                    ));
                }
            }
            Ok(Self {
                segments,
                through,
                head,
            })
        })();
        // All directory handles and temporary path values have dropped here.
        budget.release(scratch);
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                budget.release(budget.used() - start);
                return Err(error);
            }
        };
        if let Err(error) = (|| {
            budget.key_step("history capture envelope", 0)?;
            budget.charge(std::mem::size_of::<Self>() as u64 + 64)
        })() {
            drop(value);
            budget.release(budget.used() - start);
            return Err(error);
        }
        Ok(Loaded {
            value,
            charge: budget.used() - start,
        })
    }
}

/// Positional reads do not share a cursor across concurrent queries. Every kernel
/// request is at most 64 KiB, and an interrupted syscall polls controls before retry.
fn read_exact_at_checked(
    file: &File,
    mut output: &mut [u8],
    mut offset: u64,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    while !output.is_empty() {
        check()?;
        let length = output.len().min(64 << 10);
        #[cfg(unix)]
        let result = std::os::unix::fs::FileExt::read_at(file, &mut output[..length], offset);
        #[cfg(windows)]
        let result = std::os::windows::fs::FileExt::seek_read(file, &mut output[..length], offset);
        #[cfg(not(any(unix, windows)))]
        let result: std::io::Result<usize> = Err(std::io::ErrorKind::Unsupported.into());
        match result {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()),
            Ok(count) => {
                offset = offset
                    .checked_add(count as u64)
                    .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
                output = &mut output[count..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    check()
}

impl CapturedSegment {
    /// A torn or damaged suffix ends the valid prefix, as in the ordinary codec.
    /// The caller never scans past the captured file extent. Owned record memory
    /// belongs to query execution, not the earlier view-preparation allowance.
    fn body_at(
        &self,
        offset: u64,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        check()?;
        if offset.checked_add(8).is_none_or(|end| end > self.extent) {
            return Ok(None);
        }
        let mut header = [0; 8];
        read_exact_at_checked(&self.file, &mut header, offset, check)?;
        let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if length == 0
            || offset
                .checked_add(8)
                .and_then(|n| n.checked_add(length as u64))
                .is_none_or(|end| end > self.extent)
        {
            return Ok(None);
        }
        check()?;
        let mut body = Vec::new();
        body.try_reserve_exact(length).map_err(|_| {
            Error::HistoryUnsupported("history-log record allocation was refused".into())
        })?;
        let mut crc = flate2::Crc::new();
        while body.len() < length {
            check()?;
            let start = body.len();
            let end = start.saturating_add(64 << 10).min(length);
            body.resize(end, 0);
            read_exact_at_checked(
                &self.file,
                &mut body[start..end],
                offset + 8 + start as u64,
                check,
            )?;
            check()?;
            crc.update(&body[start..end]);
        }
        check()?;
        Ok((crc.sum() == u32::from_le_bytes(header[4..].try_into().unwrap())).then_some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryOptions, ReadLimit};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn history_fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(
            root.path(),
            StoreOptions {
                build: crate::builder::BuildOptions {
                    threads: 1,
                    ..Default::default()
                },
                change_log_segment_bytes: 4096,
                bulk_threshold: 0,
                change_log_bulk_max_quads: 0,
                ..Default::default()
            },
        )
        .unwrap();
        store.change_log().unwrap().set_background(false);
        store
            .set_change_log_settings(ChangeLogSettings {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
        for n in 1..=8 {
            crate::sparql::update::update(
                &store,
                &format!("INSERT DATA {{<urn:lost:{n}> <urn:p> {n}}}"),
                &Default::default(),
            )
            .unwrap();
        }
        store
            .set_change_log_settings(ChangeLogSettings::default())
            .unwrap();
        crate::sparql::update::update(
            &store,
            &format!(
                "INSERT DATA {{<urn:after:9> <urn:p> \"{}\"}}",
                "x".repeat(5000)
            ),
            &Default::default(),
        )
        .unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA {<urn:after:10> <urn:p> 10}",
            &Default::default(),
        )
        .unwrap();
        store
            .load(&[crate::io::Source::from_bytes(
                (0..32)
                    .map(|n| format!("<urn:bulk:{n}> <urn:p> {n} .\n"))
                    .collect::<String>()
                    .into_bytes(),
                crate::io::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA {<urn:after:12> <urn:p> 12}",
            &Default::default(),
        )
        .unwrap();
        store.flush_change_log().unwrap();
        assert_eq!(store.head_commit().seq, 12);
        let summary = store
            .history_changes(&crate::store::HistoryQuery {
                from: Some(crate::store::HistoryBound::Commit(11)),
                to: Some(crate::store::HistoryBound::Commit(11)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            summary.unrecorded.as_slice(),
            [Unrecorded {
                from: 11,
                to: 11,
                reason: UnrecordedReason::Bulk
            }]
        );
        (root, store)
    }

    fn log_bytes(root: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
        std::fs::read_dir(root.join(CHANGES_DIR))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    #[test]
    fn readonly_scans_match_ordinary_gap_summary_filters_limits_and_time_bounds() {
        let (root, store) = history_fixture();
        let normal = store.change_log().unwrap();
        let before = log_bytes(root.path());
        let options = HistoryOptions::default();
        for through in [6, 10, 12] {
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let loaded =
                ReadOnlyLog::capture(root.path(), normal.dataset_id, through, 12, &mut budget)
                    .unwrap();
            let reader = &loaded.value;
            for descending in [false, true] {
                for unrelated in [false, true] {
                    for limit in [None, Some(1)] {
                        for (from, to) in [
                            (0, 2),
                            (4, 6),
                            (8, 11),
                            (9, 12),
                            (11, 11),
                            (12, 12),
                            (13, 15),
                            (8, 7),
                        ] {
                            let filter = LogFilter {
                                keys: [
                                    None,
                                    None,
                                    unrelated.then(|| vec![crate::id::iri_key("urn:unmatched")]),
                                    None,
                                ],
                            };
                            let mut expected = Vec::new();
                            let holes = normal
                                .scan(
                                    from,
                                    to.min(through),
                                    &filter,
                                    descending,
                                    &mut || Ok(()),
                                    &mut |meta, add, quad| {
                                        expected.push((
                                            meta.seq,
                                            meta.timestamp_ms,
                                            meta.kind,
                                            add,
                                            quad.clone(),
                                        ));
                                        Ok(limit.is_none_or(|count| expected.len() < count))
                                    },
                                )
                                .unwrap();
                            let mut actual = Vec::new();
                            let actual_holes = reader
                                .scan(
                                    from,
                                    to,
                                    &filter,
                                    descending,
                                    &mut || Ok(()),
                                    &mut |meta, add, quad| {
                                        actual.push((
                                            meta.seq,
                                            meta.timestamp_ms,
                                            meta.kind,
                                            add,
                                            quad.clone(),
                                        ));
                                        Ok(limit.is_none_or(|count| actual.len() < count))
                                    },
                                )
                                .unwrap();
                            assert_eq!(
                                actual, expected,
                                "rows through={through} range={from}..{to} descending={descending} unrelated={unrelated} limit={limit:?}"
                            );
                            assert_eq!(
                                actual_holes, holes,
                                "holes through={through} range={from}..{to} descending={descending} unrelated={unrelated} limit={limit:?}"
                            );
                        }
                    }
                }
            }
            let earliest = store
                .resolve(&crate::history::At::Commit(9))
                .unwrap()
                .commit
                .timestamp_ms;
            for ms in [
                i64::MIN,
                earliest.saturating_sub(1),
                earliest,
                store.head_commit().timestamp_ms,
                i64::MAX,
            ] {
                assert_eq!(
                    reader.commit_at_time(ms, &mut || Ok(())).unwrap(),
                    normal.commit_at_time_checked(ms, &mut || Ok(())).unwrap(),
                    "time through={through} instant={ms}"
                );
            }
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(log_bytes(root.path()), before);
    }

    #[test]
    fn readonly_commits_match_ordinary_summaries_gaps_and_visitor_stops() {
        let (root, store) = history_fixture();
        let normal = store.change_log().unwrap();
        let before = log_bytes(root.path());
        let options = HistoryOptions::default();
        for through in [6, 10, 12] {
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let loaded =
                ReadOnlyLog::capture(root.path(), normal.dataset_id, through, 12, &mut budget)
                    .unwrap();
            for (from, to) in [
                (0, 3),
                (4, 6),
                (8, 11),
                (9, 12),
                (10, 12),
                (11, 11),
                (12, 12),
                (13, 15),
                (8, 7),
            ] {
                for limit in [None, Some(1)] {
                    let mut expected = Vec::new();
                    normal
                        .commits(from, to.min(through), &mut || Ok(()), &mut |record| {
                            expected.push(commit_value(record));
                            Ok(limit.is_none_or(|n| expected.len() < n))
                        })
                        .unwrap();
                    let mut actual = Vec::new();
                    loaded
                        .value
                        .commits(from, to, &mut || Ok(()), &mut |record| {
                            actual.push(commit_value(record));
                            Ok(limit.is_none_or(|n| actual.len() < n))
                        })
                        .unwrap();
                    assert_eq!(
                        actual, expected,
                        "through={through} range={from}..{to} limit={limit:?}"
                    );
                }
            }
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(log_bytes(root.path()), before);
    }

    fn commit_value(record: LogCommit) -> (u8, u64, Vec<(bool, QuadKey)>) {
        match record {
            LogCommit::Changes(meta, changes) => (1, meta.seq, changes),
            LogCommit::Summary(meta) => (2, meta.seq, Vec::new()),
            LogCommit::Missing => (0, 0, Vec::new()),
        }
    }

    #[test]
    fn closed_log_routes_public_queries_and_refuses_all_writer_paths() {
        let (root, store) = history_fixture();
        let normal = store.change_log().unwrap();
        let before = log_bytes(root.path());
        let options = HistoryOptions::default();
        for through in [6, 10, 12] {
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let loaded = ChangeLog::capture_read_only(
                root.path(),
                normal.dataset_id,
                through,
                12,
                normal.limits,
                normal.settings(),
                &mut budget,
            )
            .unwrap();
            let log = &loaded.value;
            assert!(log.dir.is_none() && log.root.is_none());
            assert!(log.inner.lock().file.is_none());
            assert_eq!(log.status().first, Some(1));
            assert_eq!(log.status().last, Some(through));
            assert_eq!(log.status().error, None);
            for descending in [false, true] {
                for limit in [0, 1] {
                    for (from, to) in [(1, 6), (8, 12), (9, 12), (11, 11), (13, 14)] {
                        let query = HistoryQuery {
                            from: Some(HistoryBound::Commit(from)),
                            to: Some(HistoryBound::Commit(to)),
                            descending,
                            limit,
                            ..Default::default()
                        };
                        let expected = normal.query(&query, through).unwrap();
                        // Even an overlarge requested bound cannot escape capture.
                        let actual = log.query(&query, u64::MAX).unwrap();
                        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
                    }
                }
            }
            let time_query = HistoryQuery {
                from: Some(HistoryBound::Time(i64::MIN)),
                to: Some(HistoryBound::Time(i64::MAX)),
                ..Default::default()
            };
            assert_eq!(
                format!("{:?}", log.query(&time_query, u64::MAX).unwrap()),
                format!("{:?}", normal.query(&time_query, through).unwrap())
            );
            let cancelled = HistoryQuery {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                from: Some(HistoryBound::Time(i64::MIN)),
                ..Default::default()
            };
            assert!(matches!(
                log.query(&cancelled, through),
                Err(Error::Cancelled)
            ));
            let expired = HistoryQuery {
                deadline: Some(Instant::now() - Duration::from_secs(1)),
                ..Default::default()
            };
            assert!(matches!(log.query(&expired, through), Err(Error::Timeout)));
            log.set_background(true);
            log.push(Pending {
                commit: ChangeCommit::of(&store.head_commit(), None, None),
                body: PendingBody::Keys(Vec::new()),
            });
            assert!(log.pending.lock().is_empty());
            log.flush(true).unwrap();
            assert!(matches!(
                log.enforce(None),
                Err(Error::HistoryUnsupported(_))
            ));
            assert!(matches!(
                log.remove_all(),
                Err(Error::HistoryUnsupported(_))
            ));
            assert!(matches!(
                log.set_settings(
                    ChangeLogSettings {
                        enabled: Some(false),
                        ..Default::default()
                    },
                    12
                ),
                Err(Error::HistoryUnsupported(_))
            ));
            assert_eq!(log.settings(), normal.settings());
            assert_eq!(log_bytes(root.path()), before);
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn late_capture_refusal_refunds_only_its_storage_and_preserves_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(CHANGES_DIR);
        std::fs::create_dir(&dir).unwrap();
        let id = uuid::Uuid::new_v4();
        for first in [1, 2, 3] {
            std::fs::write(dir.join(seg_name(first)), seg_header(id)).unwrap();
        }
        let before = log_bytes(root.path());
        for timeout in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let deadline = timeout.then(|| Instant::now() + Duration::from_secs(1));
            let options = HistoryOptions {
                cancel: Some(cancel.clone()),
                deadline,
            };
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(1 << 20),
                held: 0,
            })
            .unwrap();
            budget.charge(17).unwrap();
            let reached = AtomicBool::new(false);
            let observer = |stage: &str, _: usize| {
                if stage == "history capture envelope" {
                    reached.store(true, Ordering::Relaxed);
                    if let Some(deadline) = deadline {
                        std::thread::sleep(
                            deadline.saturating_duration_since(Instant::now())
                                + Duration::from_millis(2),
                        );
                    } else {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            };
            budget.observe_keys(&observer);
            let result = ReadOnlyLog::capture(root.path(), id, 3, 3, &mut budget);
            assert!(
                reached.load(Ordering::Relaxed),
                "did not exercise late envelope refusal"
            );
            match result {
                Err(Error::Timeout) if timeout => {}
                Err(Error::Cancelled) if !timeout => {}
                _ => panic!("wrong late capture refusal"),
            }
            assert_eq!(
                budget.used(),
                17,
                "late capture retained freed segment storage"
            );
            assert_eq!(log_bytes(root.path()), before);
        }
    }

    #[test]
    fn checked_history_codec_preserves_terms_metadata_and_full_record_validation() {
        let meta = Arc::new(ChangeCommit {
            seq: 9,
            timestamp_ms: 17,
            kind: CommitKind::Update,
            bulk: false,
            inserted: 1,
            deleted: 0,
            author: Some(Arc::from("é 🦀")),
            message: Some(Arc::from("retained revision")),
        });
        let key: QuadKey = [b"".as_slice(), b"<urn:s>", b"<urn:p>", b"\"value\""].map(Arc::from);
        let wire = encode(&Record::Changes(meta.clone(), vec![(true, key.clone())]));
        let Some(RecordView::Changes {
            meta: actual,
            terms,
            entries,
            count,
        }) = decode_checked(&wire[8..], &mut || Ok(())).unwrap()
        else {
            panic!("not a changes record");
        };
        assert_eq!(actual.seq, meta.seq);
        assert_eq!(actual.author, meta.author);
        assert_eq!(actual.message, meta.message);
        assert_eq!(count, 1);
        let mut cur = Cur { b: entries, i: 0 };
        assert_eq!(cur.u8(), Some(1));
        for expected in key {
            assert_eq!(terms[cur.varint().unwrap() as usize], expected);
        }
        // The final term reference is invalid. No partial RecordView is returned.
        let mut bad = wire[8..].to_vec();
        *bad.last_mut().unwrap() = 127;
        assert!(decode_checked(&bad, &mut || Ok(())).unwrap().is_none());
        let summary = encode(&Record::Summary(meta.clone()));
        assert!(
            matches!(decode_checked(&summary[8..],&mut||Ok(())).unwrap(),Some(RecordView::Summary(c)) if c.seq==meta.seq)
        );
        for padding in (64 << 10) - 4..(64 << 10) + 4 {
            let text = "a".repeat(padding) + "🦀éλ";
            assert_eq!(
                arc_text_checked(text.as_bytes(), &mut || Ok(()))
                    .unwrap()
                    .as_deref(),
                Some(text.as_str())
            );
        }
        for bad in [
            vec![0x80; 70_000],
            vec![0xff],
            vec![0xf0, 0x90, 0x80],
            vec![0xc0, 0x80],
        ] {
            assert!(arc_text_checked(&bad, &mut || Ok(())).unwrap().is_none());
        }
    }

    #[test]
    fn checked_history_payload_copy_cancels_after_the_first_chunk() {
        let bytes = vec![b'x'; 4 << 20];
        let mut calls = 0;
        let result = arc_bytes_checked(&bytes, &mut || {
            calls += 1;
            if calls == 3 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn readonly_frames_match_the_shared_codec_and_cancel_inside_large_reads() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("segment.log");
        let id = uuid::Uuid::new_v4();
        let record = encode(&Record::Gap {
            from: 3,
            through: 8,
        });
        let mut wire = seg_header(id).to_vec();
        wire.extend_from_slice(&record);
        std::fs::write(&path, &wire).unwrap();
        let segment = CapturedSegment {
            first: 3,
            extent: wire.len() as u64,
            file: File::open(&path).unwrap(),
        };
        let actual = segment
            .body_at(SEG_HEADER as u64, &mut || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(actual.as_slice(), frame(&wire, SEG_HEADER).unwrap().0);
        assert!(matches!(
            decode(&actual),
            Some(Record::Gap {
                from: 3,
                through: 8
            })
        ));
        wire[SEG_HEADER + 4] ^= 1;
        std::fs::write(&path, &wire).unwrap();
        assert!(
            segment
                .body_at(SEG_HEADER as u64, &mut || Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), wire);
        let body = vec![7; 4 << 20];
        wire = seg_header(id).to_vec();
        wire.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wire.extend_from_slice(&crc32(&body).to_le_bytes());
        wire.extend_from_slice(&body);
        std::fs::write(&path, &wire).unwrap();
        let segment = CapturedSegment {
            first: 3,
            extent: wire.len() as u64,
            file: File::open(&path).unwrap(),
        };
        let mut checks = 0;
        let result = segment.body_at(SEG_HEADER as u64, &mut || {
            checks += 1;
            // The first chunk has been read; most of this record is still unread.
            if checks == 8 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(checks, 8);
        assert_eq!(std::fs::read(&path).unwrap(), wire);
    }

    #[test]
    fn captured_history_handles_keep_exact_files_and_selected_upper_bound() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(CHANGES_DIR)).unwrap();
        let id = uuid::Uuid::new_v4();
        for first in [20, 1, 9] {
            std::fs::write(
                root.path().join(CHANGES_DIR).join(seg_name(first)),
                seg_header(id),
            )
            .unwrap();
        }
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        let loaded = ReadOnlyLog::capture(root.path(), id, 10, 10, &mut budget).unwrap();
        assert_eq!(loaded.value.through, 10);
        assert_eq!(
            loaded
                .value
                .segments
                .iter()
                .map(|s| s.first)
                .collect::<Vec<_>>(),
            [1, 9]
        );
        let path = root.path().join(CHANGES_DIR).join(seg_name(1));
        std::fs::rename(&path, path.with_extension("replaced")).unwrap();
        std::fs::write(&path, seg_header(uuid::Uuid::new_v4())).unwrap();
        assert_eq!(loaded.value.segments[0].extent, SEG_HEADER as u64);
        assert_eq!(
            loaded.value.segments[0].file.metadata().unwrap().len(),
            SEG_HEADER as u64
        );
        let mut header = [0; SEG_HEADER];
        use std::io::{Seek, SeekFrom};
        let mut descriptor = loaded.value.segments[0].file.try_clone().unwrap();
        descriptor.seek(SeekFrom::Start(0)).unwrap();
        descriptor.read_exact(&mut header).unwrap();
        assert_eq!(header, seg_header(id));
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn history_capture_refuses_and_cancels_without_sidecar_or_repair_writes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(CHANGES_DIR);
        std::fs::create_dir(&dir).unwrap();
        let id = uuid::Uuid::new_v4();
        for first in 1..10 {
            std::fs::write(dir.join(seg_name(first)), seg_header(id)).unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1024),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            ReadOnlyLog::capture(root.path(), id, 10, 10, &mut budget),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(budget.used(), 0);
        let observer = |stage: &str, at| {
            if stage == "history log segment" && at == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        assert!(matches!(
            ReadOnlyLog::capture(root.path(), id, 10, 10, &mut budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 9);
        for first in 1..10 {
            assert_eq!(
                std::fs::read(dir.join(seg_name(first))).unwrap(),
                seg_header(id)
            );
        }
    }
}

/// Copy opaque term payloads cooperatively. Allocation is owned by this query;
/// no owner capability or trust is derived from these bytes.
fn arc_bytes_checked(bytes: &[u8], check: &mut dyn FnMut() -> Result<()>) -> Result<Arc<[u8]>> {
    check()?;
    let mut owned = Arc::<[u8]>::new_uninit_slice(bytes.len());
    let output = Arc::get_mut(&mut owned).expect("new byte storage is uniquely owned");
    for (target, source) in output.chunks_mut(64 << 10).zip(bytes.chunks(64 << 10)) {
        check()?;
        for (slot, byte) in target.iter_mut().zip(source) {
            slot.write(*byte);
        }
    }
    check()?;
    // SAFETY: all bytes of the uniquely owned exact-length allocation were
    // initialized by the safe loop. An error drops MaybeUninit storage directly.
    Ok(unsafe { owned.assume_init() })
}

fn valid_utf8_checked(bytes: &[u8], check: &mut dyn FnMut() -> Result<()>) -> Result<bool> {
    let mut start = 0;
    while start < bytes.len() {
        check()?;
        let mut end = start.saturating_add(64 << 10).min(bytes.len());
        if end < bytes.len() {
            // Keep a UTF-8 code point in one validation chunk. A malformed run of
            // continuation bytes is rejected rather than making no progress.
            while end > start && bytes[end] & 0xc0 == 0x80 {
                end -= 1;
            }
        }
        if end == start || std::str::from_utf8(&bytes[start..end]).is_err() {
            return Ok(false);
        }
        start = end;
    }
    check()?;
    Ok(true)
}

fn arc_text_checked(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<Arc<str>>> {
    if !valid_utf8_checked(bytes, check)? {
        return Ok(None);
    }
    let owned = arc_bytes_checked(bytes, check)?;
    // SAFETY: the payload was validated as UTF-8 and copied without changes.
    // str and [u8] have identical element alignment and length metadata, so this
    // transfers the same Arc allocation exactly once with its existing reference
    // count and deallocation layout. No reference to the old Arc remains.
    let text = unsafe { Arc::from_raw(Arc::into_raw(owned) as *const str) };
    check()?;
    Ok(Some(text))
}

fn meta_checked(
    c: &mut Cur<'_>,
    seq: u64,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<Arc<ChangeCommit>>> {
    let Some((timestamp_ms, kind, bulk, inserted, deleted, author, message)) = (|| {
        let timestamp_ms = c.u64()? as i64;
        let kind = CommitKind::from_code(c.u8()?);
        let bulk = c.u8()? & 1 != 0;
        let inserted = c.varint()?;
        let deleted = c.varint()?;
        let n = usize::try_from(c.varint()?).ok()?;
        let author = c.bytes(n)?;
        let n = usize::try_from(c.varint()?).ok()?;
        let message = c.bytes(n)?;
        Some((timestamp_ms, kind, bulk, inserted, deleted, author, message))
    })() else {
        return Ok(None);
    };
    let author = if author.is_empty() {
        None
    } else {
        let Some(text) = arc_text_checked(author, check)? else {
            return Ok(None);
        };
        Some(text)
    };
    let message = if message.is_empty() {
        None
    } else {
        let Some(text) = arc_text_checked(message, check)? else {
            return Ok(None);
        };
        Some(text)
    };
    check()?;
    Ok(Some(Arc::new(ChangeCommit {
        seq,
        timestamp_ms,
        kind,
        bulk,
        inserted,
        deleted,
        author,
        message,
    })))
}

/// A record's table is required for positional term references. Changes remain
/// borrowed and are emitted one at a time instead of collecting all its quads.
enum RecordView<'a> {
    Changes {
        meta: Arc<ChangeCommit>,
        terms: Vec<Arc<[u8]>>,
        entries: &'a [u8],
        count: u64,
    },
    Summary(Arc<ChangeCommit>),
    Gap {
        from: u64,
        through: u64,
    },
}

fn decode_checked<'a>(
    body: &'a [u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<RecordView<'a>>> {
    check()?;
    let mut c = Cur { b: body, i: 0 };
    let Some((ty, seq)) = c.u8().zip(c.u64()) else {
        return Ok(None);
    };
    let record = match ty {
        REC_GAP => {
            let Some(from) = c.u64() else {
                return Ok(None);
            };
            if from > seq {
                return Ok(None);
            }
            RecordView::Gap { from, through: seq }
        }
        REC_SUMMARY => {
            let Some(meta) = meta_checked(&mut c, seq, check)? else {
                return Ok(None);
            };
            RecordView::Summary(meta)
        }
        REC_CHANGES => {
            let Some(meta) = meta_checked(&mut c, seq, check)? else {
                return Ok(None);
            };
            let Some(n) = c.varint().and_then(|n| usize::try_from(n).ok()) else {
                return Ok(None);
            };
            let mut terms = Vec::new();
            for _ in 0..n {
                check()?;
                let Some(length) = c.varint().and_then(|n| usize::try_from(n).ok()) else {
                    return Ok(None);
                };
                let Some(bytes) = c.bytes(length) else {
                    return Ok(None);
                };
                if terms.len() == terms.capacity() {
                    terms.try_reserve(1).map_err(|_| {
                        Error::HistoryUnsupported(
                            "history term-table allocation was refused".into(),
                        )
                    })?;
                }
                terms.push(arc_bytes_checked(bytes, check)?);
            }
            let Some(count) = c.varint() else {
                return Ok(None);
            };
            let start = c.i;
            // Validate the entire record before emitting a single row. A damaged
            // trailing reference must not expose a partial changes record.
            for _ in 0..count {
                check()?;
                if c.u8().is_none() {
                    return Ok(None);
                }
                for _ in 0..4 {
                    if c.varint().is_none_or(|n| n >= terms.len() as u64) {
                        return Ok(None);
                    }
                }
            }
            RecordView::Changes {
                meta,
                terms,
                entries: &body[start..c.i],
                count,
            }
        }
        _ => return Ok(None),
    };
    check()?;
    Ok((c.i == body.len()).then_some(record))
}

impl RecordView<'_> {
    fn first(&self) -> u64 {
        match self {
            Self::Changes { meta, .. } | Self::Summary(meta) => meta.seq,
            Self::Gap { from, .. } => *from,
        }
    }
    fn last(&self) -> u64 {
        match self {
            Self::Changes { meta, .. } | Self::Summary(meta) => meta.seq,
            Self::Gap { through, .. } => *through,
        }
    }
    fn timestamp(&self) -> Option<i64> {
        match self {
            Self::Changes { meta, .. } | Self::Summary(meta) => Some(meta.timestamp_ms),
            Self::Gap { .. } => None,
        }
    }
    fn emit(
        &self,
        filter: &LogFilter,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut Visit<'_>,
    ) -> Result<bool> {
        check()?;
        let Self::Changes {
            meta,
            terms,
            entries,
            count,
        } = self
        else {
            return Ok(true);
        };
        let mut c = Cur { b: entries, i: 0 };
        for _ in 0..*count {
            check()?;
            let add = c.u8().expect("all change entries were validated") == 1;
            let quad = std::array::from_fn(|_| {
                terms[c.varint().expect("all term references were validated") as usize].clone()
            });
            if filter.matches(&quad) && !visit(meta, add, &quad)? {
                check()?;
                return Ok(false);
            }
        }
        check()?;
        Ok(true)
    }
}

type WalkVisitor<'a> =
    dyn FnMut(usize, u64, &RecordView<'_>, &mut dyn FnMut() -> Result<()>) -> Result<bool> + 'a;

impl ReadOnlyLog {
    /// Walk each segment's valid consecutive prefix without retaining the log or
    /// copying its changes. The callback may retain only data it needs for a query.
    fn walk(
        &self,
        maximum: u64,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut WalkVisitor<'_>,
    ) -> Result<()> {
        check()?;
        for (index, segment) in self.segments.iter().enumerate() {
            check()?;
            let mut offset = SEG_HEADER as u64;
            let mut previous = None;
            while let Some(body) = segment.body_at(offset, check)? {
                let Some(record) = decode_checked(&body, check)? else {
                    break;
                };
                if previous.is_some_and(|last: u64| last.checked_add(1) != Some(record.first())) {
                    break;
                }
                if record.first() > maximum {
                    break;
                }
                let last = record.last();
                if !visit(index, offset, &record, check)? {
                    check()?;
                    return Ok(());
                }
                check()?;
                previous = Some(last);
                offset = offset
                    .checked_add(8 + body.len() as u64)
                    .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
            }
        }
        check()
    }

    pub(super) fn commit_at_time(
        &self,
        ms: i64,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<u64>> {
        let mut best = None;
        self.walk(self.head, check, &mut |_, _, record, _| {
            // Match the ordinary index's last recorded commit at or before time;
            // gap records have no timestamp and cannot manufacture a time bound.
            if record.timestamp().is_some_and(|timestamp| timestamp <= ms) {
                best = Some(record.last());
            }
            Ok(true)
        })?;
        Ok(best)
    }
}

fn push_checked<T>(
    values: &mut Vec<T>,
    value: T,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    if values.len() == values.capacity() {
        values.try_reserve(1).map_err(|_| {
            Error::HistoryUnsupported("history-query metadata allocation was refused".into())
        })?;
    }
    values.push(value);
    check()
}

fn sort_checked<T>(
    values: &mut [T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let mut sift = |values: &mut [T], mut root: usize, end: usize| -> Result<()> {
        loop {
            check()?;
            let left = root.saturating_mul(2).saturating_add(1);
            if left >= end {
                return Ok(());
            }
            let child = if left + 1 < end && compare(&values[left], &values[left + 1]).is_lt() {
                left + 1
            } else {
                left
            };
            if !compare(&values[root], &values[child]).is_lt() {
                return Ok(());
            }
            values.swap(root, child);
            root = child;
        }
    };
    let len = values.len();
    for root in (0..len / 2).rev() {
        sift(values, root, len)?;
    }
    for end in (1..len).rev() {
        values.swap(0, end);
        sift(values, 0, end)?;
    }
    check()
}

fn normalize_holes(
    holes: &mut Vec<Unrecorded>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    sort_checked(holes, |a, b| a.from.cmp(&b.from), check)?;
    let mut keep = 0;
    for index in 0..holes.len() {
        check()?;
        let next = holes[index];
        if keep > 0
            && holes[keep - 1].reason == next.reason
            && next.from <= holes[keep - 1].to.saturating_add(1)
        {
            holes[keep - 1].to = holes[keep - 1].to.max(next.to);
        } else {
            holes[keep] = next;
            keep += 1;
        }
    }
    holes.truncate(keep);
    check()
}

#[derive(Clone, Copy)]
struct RecordOffset {
    segment: usize,
    offset: u64,
}

impl ReadOnlyLog {
    pub(super) fn scan(
        &self,
        from: u64,
        to: u64,
        filter: &LogFilter,
        descending: bool,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut Visit<'_>,
    ) -> Result<Vec<Unrecorded>> {
        check()?;
        let to = to.min(self.through);
        if from > to {
            return Ok(Vec::new());
        }
        // These are query-execution temporaries, never retained on the view.
        let mut bounds = Vec::new();
        for _ in &self.segments {
            push_checked(&mut bounds, None, check)?;
        }
        let mut local_holes = Vec::new();
        let mut reverse = Vec::new();
        let mut stopped = false;
        self.walk(
            self.through,
            check,
            &mut |segment, offset, record, control| {
                bounds[segment] = Some(record.last());
                if record.last() < from || record.first() > to {
                    return Ok(true);
                }
                let reason = match record {
                    RecordView::Summary(_) => Some(UnrecordedReason::Bulk),
                    RecordView::Gap { .. } => Some(UnrecordedReason::Gap),
                    RecordView::Changes { .. } => None,
                };
                if let Some(reason) = reason {
                    push_checked(
                        &mut local_holes,
                        (
                            segment,
                            Unrecorded {
                                from: from.max(record.first()),
                                to: to.min(record.last()),
                                reason,
                            },
                        ),
                        control,
                    )?;
                } else if descending {
                    push_checked(&mut reverse, RecordOffset { segment, offset }, control)?;
                } else if !stopped && !record.emit(filter, control, visit)? {
                    stopped = true;
                }
                Ok(true)
            },
        )?;
        let mut holes = Vec::new();
        let (mut first, mut last) = (None, None);
        for (index, bound) in bounds.iter().enumerate() {
            check()?;
            if bound.is_some() {
                if first.is_none() {
                    first = Some(index);
                }
                last = Some(index);
            }
        }
        let (Some(first), Some(last)) = (first, last) else {
            push_checked(
                &mut holes,
                Unrecorded {
                    from,
                    to,
                    reason: UnrecordedReason::BeforeLog,
                },
                check,
            )?;
            return Ok(holes);
        };
        let oldest = self.segments[first].first;
        let newest = bounds[last].expect("a covered last segment");
        if from < oldest {
            push_checked(
                &mut holes,
                Unrecorded {
                    from,
                    to: to.min(oldest - 1),
                    reason: UnrecordedReason::BeforeLog,
                },
                check,
            )?;
        }
        if to > newest {
            push_checked(
                &mut holes,
                Unrecorded {
                    from: from.max(newest.saturating_add(1)),
                    to,
                    reason: UnrecordedReason::Gap,
                },
                check,
            )?;
        }
        let mut previous: Option<u64> = None;
        for (index, bound) in bounds.iter().enumerate() {
            check()?;
            let Some(end) = bound else {
                continue;
            };
            let start = self.segments[index].first;
            if let Some(prev) = previous
                && prev < start.saturating_sub(1)
            {
                let lower = from.max(prev.saturating_add(1));
                let upper = to.min(start - 1);
                if lower <= upper {
                    push_checked(
                        &mut local_holes,
                        (
                            index,
                            Unrecorded {
                                from: lower,
                                to: upper,
                                reason: UnrecordedReason::Gap,
                            },
                        ),
                        check,
                    )?;
                }
            }
            previous = Some(*end);
        }
        let mut stop_segment = None;
        if descending {
            for item in reverse.iter().rev() {
                check()?;
                let body = self.segments[item.segment]
                    .body_at(item.offset, check)?
                    .ok_or_else(|| {
                        Error::Corrupt("captured history record is no longer valid".into())
                    })?;
                let record = decode_checked(&body, check)?.ok_or_else(|| {
                    Error::Corrupt("captured history record is no longer valid".into())
                })?;
                if !record.emit(filter, check, visit)? {
                    stop_segment = Some(item.segment);
                    break;
                }
            }
        }
        for (segment, hole) in local_holes {
            check()?;
            // Ordinary descending scans stop before collecting older segments'
            // holes once their visitor stops; global boundary holes stay visible.
            if stop_segment.is_none_or(|stop| segment >= stop) {
                push_checked(&mut holes, hole, check)?;
            }
        }
        normalize_holes(&mut holes, check)?;
        Ok(holes)
    }
}

impl ReadOnlyLog {
    /// Read complete commit records in ascending order for the existing diff path.
    /// The copied changes belong only to this invocation and are dropped before the
    /// next record is decoded. Missing records are reported with the existing
    /// commit visitor's continuation semantics.
    pub(super) fn commits(
        &self,
        from: u64,
        to: u64,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut dyn FnMut(LogCommit) -> Result<bool>,
    ) -> Result<()> {
        check()?;
        let to = to.min(self.through);
        if from > to {
            return Ok(());
        }
        let mut next = Some(from);
        let mut stopped = false;
        self.walk(to, check, &mut |_, _, record, control| {
            let Some(wanted) = next else {
                stopped = true;
                return Ok(false);
            };
            if record.last() < wanted {
                return Ok(true);
            }
            if record.first() > wanted {
                control()?;
                visit(LogCommit::Missing)?;
                control()?;
                stopped = true;
                return Ok(false);
            }
            let item = match record {
                RecordView::Changes { meta, .. } => {
                    let mut changes = Vec::new();
                    record.emit(
                        &LogFilter {
                            keys: [None, None, None, None],
                        },
                        control,
                        &mut |_, add, quad| {
                            // Arc clones retain exactly this record's term table;
                            // admission precedes any change-vector growth.
                            if changes.len() == changes.capacity() {
                                changes.try_reserve(1).map_err(|_| {
                                    Error::HistoryUnsupported(
                                        "history commit allocation was refused".into(),
                                    )
                                })?;
                            }
                            changes.push((add, quad.clone()));
                            Ok(true)
                        },
                    )?;
                    LogCommit::Changes(meta.clone(), changes)
                }
                RecordView::Summary(meta) => LogCommit::Summary(meta.clone()),
                RecordView::Gap { .. } => LogCommit::Missing,
            };
            next = record.last().checked_add(1);
            control()?;
            if !visit(item)? || next.is_none_or(|n| n > to) {
                control()?;
                stopped = true;
                return Ok(false);
            }
            control()?;
            Ok(true)
        })?;
        if !stopped && next.is_some_and(|n| n <= to) {
            check()?;
            visit(LogCommit::Missing)?;
        }
        check()
    }
}

impl ChangeLog {
    pub(super) fn require_writable(&self) -> Result<()> {
        if self.readonly.is_some() {
            return Err(Error::HistoryUnsupported(
                "a retained-origin history attachment is read-only".into(),
            ));
        }
        Ok(())
    }

    /// Capture only the owning branch's descriptors. Settings have already been
    /// decoded under the same preparation allowance by the origin resolver.
    pub(crate) fn capture_read_only(
        root: &Path,
        dataset: uuid::Uuid,
        through: u64,
        head: u64,
        limits: LogLimits,
        settings: ChangeLogSettings,
        budget: &mut ColdBudget<'_>,
    ) -> Result<Loaded<Arc<Self>>> {
        let start = budget.used();
        let reader = ReadOnlyLog::capture(root, dataset, through, head, budget)?;
        let envelope = std::mem::size_of::<Self>() as u64 + 64;
        if let Err(error) = budget.charge(envelope) {
            drop(reader.value);
            budget.release(reader.charge);
            return Err(error);
        }
        let value = Self::new(
            dataset,
            None,
            limits,
            settings,
            Inner::empty(),
            Some(reader.value),
        );
        value.background.store(false, Ordering::Relaxed);
        Ok(Loaded {
            value,
            charge: budget.used() - start,
        })
    }
}

impl ReadOnlyLog {
    pub(super) fn coverage(&self) -> Result<(Option<u64>, Option<u64>)> {
        let (mut first, mut last) = (None, None);
        self.walk(self.through, &mut || Ok(()), &mut |_, _, record, _| {
            first.get_or_insert(record.first());
            last = Some(record.last().min(self.through));
            Ok(true)
        })?;
        Ok((first, last))
    }
}

impl ChangeLog {
    /// Read the same closed settings without a writer or startup preflight.
    pub(crate) fn settings_controlled(
        root: &Path,
        budget: &mut ColdBudget<'_>,
    ) -> Result<ChangeLogSettings> {
        let start = budget.used();
        budget.charge(
            (root.as_os_str().len() as u64)
                .saturating_mul(4)
                .saturating_add(2048),
        )?;
        let result = (|| {
            let bytes = match budget.read_file(&root.join(CHANGE_LOG_FILE)) {
                Ok(bytes) => bytes,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(ChangeLogSettings::default());
                }
                Err(error) => return Err(error),
            };
            let mut raw: Loaded<serde_json::Value> = budget.json(&bytes)?;
            if raw
                .value
                .get("format")
                .is_some_and(|value| !value.is_null() && !value.is_number())
            {
                return Err(Error::Corrupt(
                    "changelog.json: format is not a version".into(),
                ));
            }
            crate::formats::check_settings(CHANGE_LOG_FILE, &raw.value)?;
            if let Some(object) = raw.value.as_object_mut() {
                object.entry("format").or_insert(1.into());
            }
            let file: SettingsFile = budget.value(raw.value)?;
            budget.check()?;
            Ok(file.settings)
        })();
        budget.release(budget.used() - start);
        result.and_then(|value| {
            budget.check()?;
            Ok(value)
        })
    }
}
