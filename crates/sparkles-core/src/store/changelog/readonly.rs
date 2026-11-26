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
        budget.check(0)?;
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
                budget.check(0)?;
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
                budget.check(0)?;
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

impl CapturedSegment {
    /// A torn or damaged suffix ends the valid prefix, as in the ordinary codec.
    /// The caller never scans past the captured file extent. Owned record memory
    /// belongs to query execution, not the earlier view-preparation allowance.
    fn body_at<C: Control>(&self, offset: u64, control: &C) -> Result<Option<Vec<u8>>> {
        read_file_frame_with::<true, _>(
            &self.file,
            offset,
            Some(self.extent),
            false,
            false,
            control,
            &mut |_| Ok(()),
        )
    }

    /// [`CapturedSegment::body_at`], telling `admit` the body's length before the body
    /// is allocated (item 829).
    fn body_at_admitted<C: Control>(
        &self,
        offset: u64,
        control: &C,
        admit: &mut impl FnMut(u64) -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        read_file_frame_with::<true, _>(
            &self.file,
            offset,
            Some(self.extent),
            false,
            false,
            control,
            admit,
        )
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
                                    true,
                                    false,
                                    None,
                                    &mut || Ok(()),
                                    None,
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
                    normal
                        .commit_at_time_checked(ms, &mut || Ok(()), None)
                        .unwrap(),
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
        }) = decode_view_with(&wire[8..], &Unlimited, false).unwrap()
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
        assert!(decode_view_with(&bad, &Unlimited, false).unwrap().is_none());
        let summary = encode(&Record::Summary(meta.clone()));
        assert!(
            matches!(decode_view_with(&summary[8..], &Unlimited, false).unwrap(), Some(RecordView::Summary(c)) if c.seq == meta.seq)
        );
        for padding in (64 << 10) - 4..(64 << 10) + 4 {
            let text = "a".repeat(padding) + "🦀éλ";
            let mut ok = || Ok(());
            assert_eq!(
                arc_text_with(text.as_bytes(), &FnMutControl::new(&mut ok))
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
            let mut ok = || Ok(());
            assert!(
                arc_text_with(&bad, &FnMutControl::new(&mut ok))
                    .unwrap()
                    .is_none()
            );
        }
    }

    fn timestamp_meta(seq: u64, timestamp_ms: i64) -> Arc<ChangeCommit> {
        Arc::new(ChangeCommit {
            seq,
            timestamp_ms,
            kind: CommitKind::Update,
            bulk: false,
            inserted: 2,
            deleted: 0,
            author: Some(Arc::from("test author")),
            message: Some(Arc::from("é 🦀")),
        })
    }

    fn timestamp_changes(seq: u64, timestamp_ms: i64, rows: usize) -> Vec<u8> {
        let quad: QuadKey = [b"".as_slice(), b"<urn:s>", b"<urn:p>", b"\"value\""].map(Arc::from);
        encode(&Record::Changes(
            timestamp_meta(seq, timestamp_ms),
            vec![(true, quad); rows],
        ))
    }

    fn timestamp_segment(root: &Path, first: u64, records: &[&[u8]]) -> CapturedSegment {
        let path = root.join(format!("{first}.log"));
        let mut wire = seg_header(uuid::Uuid::nil()).to_vec();
        for record in records {
            wire.extend_from_slice(record);
        }
        std::fs::write(&path, &wire).unwrap();
        CapturedSegment {
            first,
            extent: wire.len() as u64,
            file: File::open(&path).unwrap(),
        }
    }

    #[test]
    fn timestamp_only_lookup_matches_owned_walk_without_building_term_owners() {
        use std::{cell::Cell, rc::Rc};

        let root = tempfile::tempdir().unwrap();
        let gap = encode(&Record::Gap {
            from: 3,
            through: 4,
        });
        let first = timestamp_changes(5, i64::MIN, 1);
        let tied = timestamp_changes(6, i64::MIN, 1);
        let at_head = encode(&Record::Summary(timestamp_meta(7, i64::MAX)));
        let after_head = timestamp_changes(8, i64::MAX, 1);
        let log = ReadOnlyLog {
            segments: vec![timestamp_segment(
                root.path(),
                3,
                &[&gap, &first, &tied, &at_head, &after_head],
            )],
            through: 5,
            head: 7,
        };
        let owned = Rc::new(Cell::new(0));
        let witness = owned.clone();
        let _observer = open_tests::Observe::new(move |event| {
            if event == open_tests::Event::ReadOnlyTermOwned {
                witness.set(witness.get() + 1);
            }
        });
        for (ms, expected) in [(i64::MIN, 6), (i64::MAX - 1, 6), (i64::MAX, 7)] {
            // Exercise the signed base's full-record walk. It even validates the
            // first record past head before rejecting it by sequence.
            let mut previous = None;
            log.walk(log.head, &Unlimited, &mut |_, _, record| {
                if record.timestamp().is_some_and(|time| time <= ms) {
                    previous = Some(record.last());
                }
                Ok(true)
            })
            .unwrap();
            assert_eq!(owned.replace(0), 12);
            assert_eq!(previous, Some(expected));
            assert_eq!(log.commit_at_time(ms, &mut || Ok(())).unwrap(), previous);
            assert_eq!(owned.get(), 0);
        }
    }

    #[test]
    fn timestamp_only_lookup_rejects_malformed_crc_valid_suffixes() {
        use crate::task::Control;
        use std::{cell::Cell, rc::Rc};

        let root = tempfile::tempdir().unwrap();
        let first = timestamp_changes(4, 40, 1);
        let good = timestamp_changes(5, 50, 2);
        let after = encode(&Record::Summary(timestamp_meta(6, 60)));
        let next_segment = encode(&Record::Summary(timestamp_meta(7, 70)));
        let log = |middle: &[u8]| ReadOnlyLog {
            segments: vec![
                timestamp_segment(root.path(), 4, &[&first, middle, &after]),
                timestamp_segment(root.path(), 7, &[&next_segment]),
            ],
            through: 7,
            head: 7,
        };
        assert_eq!(
            log(&good).commit_at_time(60, &mut || Ok(())).unwrap(),
            Some(6)
        );

        let mut meta = Cur {
            b: &good[8..],
            i: 9,
        };
        assert!(
            decode_meta_with(&mut meta, 5, &Unlimited)
                .unwrap()
                .is_some()
        );
        assert_eq!(meta.varint(), Some(4));
        let term_length = meta.i;
        let author = meta
            .b
            .windows(b"test author".len())
            .position(|w| w == b"test author")
            .unwrap();
        for damage in [
            "reference",
            "truncated",
            "trailing",
            "metadata",
            "term length",
            "kind",
            "sequence",
        ] {
            let mut body = good[8..].to_vec();
            match damage {
                "reference" => *body.last_mut().unwrap() = 127,
                "truncated" => {
                    body.pop().unwrap();
                }
                "trailing" => body.push(0),
                "metadata" => body[author] = 0xff,
                "term length" => body[term_length] = 127,
                "kind" => body[0] = 0xff,
                "sequence" => body[1..9].copy_from_slice(&6_u64.to_le_bytes()),
                _ => unreachable!(),
            }
            let mut wire = (body.len() as u32).to_le_bytes().to_vec();
            wire.extend_from_slice(&crc32(&body).to_le_bytes());
            wire.extend_from_slice(&body);
            assert_eq!(frame(&wire, 0).unwrap().0, body.as_slice(), "{damage}");
            let log = log(&wire);
            assert_eq!(
                log.commit_at_time(39, &mut || Ok(())).unwrap(),
                None,
                "{damage}"
            );
            assert_eq!(
                log.commit_at_time(60, &mut || Ok(())).unwrap(),
                Some(4),
                "{damage}"
            );
            assert_eq!(
                log.commit_at_time(70, &mut || Ok(())).unwrap(),
                Some(7),
                "{damage}"
            );

            let mut rows = Vec::new();
            log.scan(
                4,
                7,
                &LogFilter {
                    keys: [None, None, None, None],
                },
                false,
                &mut || Ok(()),
                &mut |meta, _, _| {
                    rows.push(meta.seq);
                    Ok(true)
                },
            )
            .unwrap();
            assert_eq!(rows, [4], "{damage} exposed rows from a rejected record");

            if damage == "reference" {
                let flag = Arc::new(AtomicBool::new(false));
                let control = Control::with_cancel(flag.clone());
                let refs = Rc::new(Cell::new(0));
                let witness = refs.clone();
                let _observer = open_tests::Observe::new(move |event| {
                    if event == open_tests::Event::ReadOnlyReference(5) {
                        witness.set(witness.get() + 1);
                        flag.store(true, Ordering::Relaxed);
                    }
                });
                assert!(matches!(
                    log.commit_at_time(70, &mut || control.check()),
                    Err(Error::Cancelled)
                ));
                assert_eq!(refs.get(), 1, "the bad reference followed the stop");
            }
        }
    }

    #[test]
    fn checked_history_payload_copy_cancels_after_the_first_chunk() {
        let bytes = vec![b'x'; 4 << 20];
        let mut calls = 0;
        let mut check = || {
            calls += 1;
            if calls == 3 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        };
        let result = arc_bytes_with(&bytes, &FnMutControl::new(&mut check));
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
        let mut ok = || Ok(());
        let actual = segment
            .body_at(SEG_HEADER as u64, &FnMutControl::new(&mut ok))
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
        let mut ok = || Ok(());
        assert!(
            segment
                .body_at(SEG_HEADER as u64, &FnMutControl::new(&mut ok))
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
        let mut stop = || {
            checks += 1;
            // The first chunk has been read; most of this record is still unread.
            if checks == 8 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        };
        let result = segment.body_at(SEG_HEADER as u64, &FnMutControl::new(&mut stop));
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

    #[test]
    fn readonly_status_reports_refused_frame_allocation_instead_of_aborting() {
        let root = tempfile::tempdir().unwrap();
        let rec = timestamp_changes(1, 100, 1);
        let segment = timestamp_segment(root.path(), 1, &[&rec]);
        let reader = ReadOnlyLog {
            segments: vec![segment],
            through: 1,
            head: 1,
        };
        let log = ChangeLog::new(
            uuid::Uuid::nil(),
            None,
            LogLimits {
                segment_bytes: 64 << 20,
                default_max_bytes: 256 << 20,
                default_on: true,
                bulk_max_quads: 100_000,
            },
            ChangeLogSettings {
                enabled: Some(true),
                ..Default::default()
            },
            Inner::empty(),
            Some(reader),
        );

        // Under Unlimited, status() walks captured segments through fallible frame
        // reservation and maps refusal into ChangeLogStatus.error instead of aborting.
        {
            let _refuse = RefuseFrameAlloc::new(1);
            let status = log.status();
            assert_eq!(status.first, None);
            assert_eq!(status.last, None);
            let err = status
                .error
                .as_deref()
                .expect("refused frame allocation must surface in ChangeLogStatus.error");
            assert!(
                err.contains("history-log record allocation was refused"),
                "unexpected status error: {err}"
            );
        }

        // Normal status succeeds once allocation is allowed.
        let ok = log.status();
        assert_eq!(ok.first, Some(1));
        assert_eq!(ok.last, Some(1));
        assert_eq!(ok.error, None);

        // A bad-CRC frame within the captured extent still hits fallible reservation
        // before CRC verification or decoding.
        let mut bad_crc = rec;
        bad_crc[4] ^= 1;
        let bad_seg = timestamp_segment(root.path(), 2, &[&bad_crc]);
        let _refuse = RefuseFrameAlloc::new(1);
        assert!(matches!(
            bad_seg.body_at(SEG_HEADER as u64, &Unlimited),
            Err(Error::HistoryUnsupported(_))
        ));
    }

    #[test]
    fn readonly_scan_preserves_from_only_hole_sort_on_same_start_overlap() {
        let root = tempfile::tempdir().unwrap();
        let gap_a = encode(&Record::Gap {
            from: 10,
            through: 12,
        });
        let gap_b = encode(&Record::Gap {
            from: 10,
            through: 12,
        });
        let bulk = encode(&Record::Summary(timestamp_meta(10, 100)));
        let ch11 = timestamp_changes(11, 110, 1);
        let ch12 = timestamp_changes(12, 120, 1);
        let log = ReadOnlyLog {
            segments: vec![
                timestamp_segment(root.path(), 10, &[&gap_a]),
                timestamp_segment(root.path(), 11, &[&gap_b]),
                timestamp_segment(root.path(), 12, &[&bulk, &ch11, &ch12]),
            ],
            through: 12,
            head: 12,
        };
        let holes = log
            .scan(
                10,
                12,
                &LogFilter {
                    keys: [None, None, None, None],
                },
                false,
                &mut || Ok(()),
                &mut |_, _, _| Ok(true),
            )
            .unwrap();
        assert_eq!(
            holes,
            vec![
                Unrecorded {
                    from: 10,
                    to: 12,
                    reason: UnrecordedReason::Gap,
                },
                Unrecorded {
                    from: 10,
                    to: 10,
                    reason: UnrecordedReason::Bulk,
                },
                Unrecorded {
                    from: 10,
                    to: 12,
                    reason: UnrecordedReason::Gap,
                },
            ]
        );
    }
}

/// A record's table is required for positional term references. Changes remain
/// borrowed and are emitted one at a time instead of collecting all its quads.
enum RecordView<'a, Terms = Vec<Arc<[u8]>>> {
    Changes {
        meta: Arc<ChangeCommit>,
        terms: Terms,
        entries: &'a [u8],
        count: u64,
    },
    Summary(Arc<ChangeCommit>),
    Gap {
        from: u64,
        through: u64,
    },
}

type RecordVisitor<'a, Terms> = dyn FnMut(usize, u64, &RecordView<'_, Terms>) -> Result<bool> + 'a;

/// Validates all change entries in place and returns the borrowed slice `&'a [u8]` without
/// allocating a changes vector.
fn validate_borrowed_entries<'a, C: Control>(
    #[allow(unused_variables)] seq: u64,
    term_count: usize,
    count: usize,
    c: &mut Cur<'a>,
    control: &C,
) -> Result<Option<&'a [u8]>> {
    let start = c.i;
    // Validate the entire record before emitting a single row. A damaged
    // trailing reference must not expose a partial changes record.
    for _ in 0..count {
        control.check(0)?;
        #[cfg(test)]
        open_tests::observe(open_tests::Event::ReadOnlyReference(seq));
        if c.u8().is_none() {
            return Ok(None);
        }
        for _ in 0..4 {
            if c.varint()
                .is_none_or(|reference| reference >= term_count as u64)
            {
                return Ok(None);
            }
        }
    }
    Ok(Some(&c.b[start..c.i]))
}

struct ViewRecordSink {
    exact: bool,
}

impl<'a> RecordSink<'a> for ViewRecordSink {
    type Output = RecordView<'a>;
    type Terms = Vec<Arc<[u8]>>;

    fn make_terms<C: Control>(&mut self, count: usize, _control: &C) -> Result<Self::Terms> {
        let mut terms = Vec::new();
        if self.exact {
            terms.try_reserve_exact(count).map_err(|_| {
                Error::HistoryUnsupported("history term-table allocation was refused".into())
            })?;
        }
        Ok(terms)
    }

    fn push_term<C: Control>(
        &mut self,
        terms: &mut Self::Terms,
        _idx: usize,
        raw: &[u8],
        control: &C,
    ) -> Result<()> {
        control.check(0)?;
        if terms.len() == terms.capacity() {
            terms.try_reserve(1).map_err(|_| {
                Error::HistoryUnsupported("history term-table allocation was refused".into())
            })?;
        }
        terms.push(arc_bytes_with(raw, control)?);
        #[cfg(test)]
        open_tests::observe(open_tests::Event::ReadOnlyTermOwned);
        Ok(())
    }

    fn finish_changes<C: Control>(
        self,
        meta: Arc<ChangeCommit>,
        terms: Self::Terms,
        term_count: usize,
        count: usize,
        c: &mut Cur<'a>,
        control: &C,
    ) -> Result<Option<Self::Output>> {
        let Some(entries) = validate_borrowed_entries(meta.seq, term_count, count, c, control)?
        else {
            return Ok(None);
        };
        Ok(Some(RecordView::Changes {
            meta,
            terms,
            entries,
            count: count as u64,
        }))
    }

    fn finish_summary(self, meta: Arc<ChangeCommit>) -> Self::Output {
        RecordView::Summary(meta)
    }

    fn finish_gap(self, from: u64, through: u64) -> Self::Output {
        RecordView::Gap { from, through }
    }
}

struct TimestampRecordSink;

impl<'a> RecordSink<'a> for TimestampRecordSink {
    type Output = RecordView<'a, ()>;
    type Terms = ();

    fn make_terms<C: Control>(&mut self, _count: usize, _control: &C) -> Result<Self::Terms> {
        Ok(())
    }

    fn push_term<C: Control>(
        &mut self,
        _terms: &mut Self::Terms,
        _idx: usize,
        _raw: &[u8],
        control: &C,
    ) -> Result<()> {
        // Opaque terms only need bounds checks. The frame reader already polls while
        // reading and checksumming their bytes, and decoding still polls per term.
        control.check(0)
    }

    fn finish_changes<C: Control>(
        self,
        meta: Arc<ChangeCommit>,
        terms: Self::Terms,
        term_count: usize,
        count: usize,
        c: &mut Cur<'a>,
        control: &C,
    ) -> Result<Option<Self::Output>> {
        let Some(entries) = validate_borrowed_entries(meta.seq, term_count, count, c, control)?
        else {
            return Ok(None);
        };
        Ok(Some(RecordView::Changes {
            meta,
            terms,
            entries,
            count: count as u64,
        }))
    }

    fn finish_summary(self, meta: Arc<ChangeCommit>) -> Self::Output {
        RecordView::Summary(meta)
    }

    fn finish_gap(self, from: u64, through: u64) -> Self::Output {
        RecordView::Gap { from, through }
    }
}

fn decode_view_with<'a, C: Control>(
    body: &'a [u8],
    control: &C,
    exact: bool,
) -> Result<Option<RecordView<'a>>> {
    decode_record_with(body, control, ViewRecordSink { exact })
}

fn decode_timestamp_with<'a, C: Control>(
    body: &'a [u8],
    control: &C,
) -> Result<Option<RecordView<'a, ()>>> {
    decode_record_with(body, control, TimestampRecordSink)
}

impl<Terms> RecordView<'_, Terms> {
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
}

impl RecordView<'_> {
    fn emit<C: Control>(
        &self,
        filter: &LogFilter,
        control: &C,
        visit: &mut Visit<'_>,
    ) -> Result<bool> {
        control.check(0)?;
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
            control.check(0)?;
            let add = c.u8().expect("all change entries were validated") == 1;
            let quad = std::array::from_fn(|_| {
                terms[c.varint().expect("all term references were validated") as usize].clone()
            });
            if filter.matches(&quad) && !visit(meta, add, &quad)? {
                control.check(0)?;
                return Ok(false);
            }
        }
        control.check(0)?;
        Ok(true)
    }
}

impl ReadOnlyLog {
    /// Walk each segment's valid consecutive prefix without retaining the log or
    /// copying its changes. The callback may retain only data it needs for a query.
    fn walk<C: Control>(
        &self,
        maximum: u64,
        control: &C,
        visit: &mut dyn FnMut(usize, u64, &RecordView<'_>) -> Result<bool>,
    ) -> Result<()> {
        self.walk_with(
            maximum,
            control,
            |body, c| decode_view_with(body, c, false),
            visit,
        )
    }

    fn walk_with<C: Control, Terms>(
        &self,
        maximum: u64,
        control: &C,
        decode: impl for<'a> Fn(&'a [u8], &C) -> Result<Option<RecordView<'a, Terms>>>,
        visit: &mut RecordVisitor<'_, Terms>,
    ) -> Result<()> {
        control.check(0)?;
        for (index, segment) in self.segments.iter().enumerate() {
            control.check(0)?;
            let mut offset = SEG_HEADER as u64;
            let mut previous = None;
            while let Some(body) = segment.body_at(offset, control)? {
                let Some(record) = decode(&body, control)? else {
                    break;
                };
                if previous.is_some_and(|last: u64| last.checked_add(1) != Some(record.first())) {
                    break;
                }
                if record.first() > maximum {
                    break;
                }
                let last = record.last();
                if !visit(index, offset, &record)? {
                    control.check(0)?;
                    return Ok(());
                }
                control.check(0)?;
                previous = Some(last);
                offset = offset
                    .checked_add(8 + body.len() as u64)
                    .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
            }
        }
        control.check(0)
    }

    pub(super) fn commit_at_time(
        &self,
        ms: i64,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<u64>> {
        let control = FnMutControl::new(check);
        #[cfg(test)]
        open_tests::observe(open_tests::Event::ReadOnlyTimestamp(ms));
        let mut best = None;
        self.walk_with(
            self.head,
            &control,
            decode_timestamp_with,
            &mut |_, _, record| {
                // Match the ordinary index's last recorded commit at or before time;
                // gap records have no timestamp and cannot manufacture a time bound.
                if record.timestamp().is_some_and(|timestamp| timestamp <= ms) {
                    best = Some(record.last());
                }
                Ok(true)
            },
        )?;
        Ok(best)
    }
}

fn push_with<C: Control, T>(values: &mut Vec<T>, value: T, control: &C) -> Result<()> {
    control.check(0)?;
    if values.len() == values.capacity() {
        values.try_reserve(1).map_err(|_| {
            Error::HistoryUnsupported("history-query metadata allocation was refused".into())
        })?;
    }
    values.push(value);
    control.check(0)
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
        let control = FnMutControl::new(check);
        control.check(0)?;
        let to = to.min(self.through);
        if from > to {
            return Ok(Vec::new());
        }
        // These are query-execution temporaries, never retained on the view.
        let mut bounds = Vec::new();
        for _ in &self.segments {
            push_with(&mut bounds, None, &control)?;
        }
        let mut local_holes = Vec::new();
        let mut reverse = Vec::new();
        let mut stopped = false;
        self.walk(self.through, &control, &mut |segment, offset, record| {
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
                push_with(
                    &mut local_holes,
                    (
                        segment,
                        Unrecorded {
                            from: from.max(record.first()),
                            to: to.min(record.last()),
                            reason,
                        },
                    ),
                    &control,
                )?;
            } else if descending {
                push_with(&mut reverse, RecordOffset { segment, offset }, &control)?;
            } else if !stopped && !record.emit(filter, &control, visit)? {
                stopped = true;
            }
            Ok(true)
        })?;
        let mut holes = Vec::new();
        let (mut first, mut last) = (None, None);
        for (index, bound) in bounds.iter().enumerate() {
            control.check(0)?;
            if bound.is_some() {
                if first.is_none() {
                    first = Some(index);
                }
                last = Some(index);
            }
        }
        let (Some(first), Some(last)) = (first, last) else {
            push_with(
                &mut holes,
                Unrecorded {
                    from,
                    to,
                    reason: UnrecordedReason::BeforeLog,
                },
                &control,
            )?;
            return Ok(holes);
        };
        let oldest = self.segments[first].first;
        let newest = bounds[last].expect("a covered last segment");
        if from < oldest {
            push_with(
                &mut holes,
                Unrecorded {
                    from,
                    to: to.min(oldest - 1),
                    reason: UnrecordedReason::BeforeLog,
                },
                &control,
            )?;
        }
        if to > newest {
            push_with(
                &mut holes,
                Unrecorded {
                    from: from.max(newest.saturating_add(1)),
                    to,
                    reason: UnrecordedReason::Gap,
                },
                &control,
            )?;
        }
        let mut previous: Option<u64> = None;
        for (index, bound) in bounds.iter().enumerate() {
            control.check(0)?;
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
                    push_with(
                        &mut local_holes,
                        (
                            index,
                            Unrecorded {
                                from: lower,
                                to: upper,
                                reason: UnrecordedReason::Gap,
                            },
                        ),
                        &control,
                    )?;
                }
            }
            previous = Some(*end);
        }
        let mut stop_segment = None;
        if descending {
            for item in reverse.iter().rev() {
                control.check(0)?;
                let body = self.segments[item.segment]
                    .body_at(item.offset, &control)?
                    .ok_or_else(|| {
                        Error::Corrupt("captured history record is no longer valid".into())
                    })?;
                let record = decode_view_with(&body, &control, false)?.ok_or_else(|| {
                    Error::Corrupt("captured history record is no longer valid".into())
                })?;
                if !record.emit(filter, &control, visit)? {
                    stop_segment = Some(item.segment);
                    break;
                }
            }
        }
        for (segment, hole) in local_holes {
            control.check(0)?;
            // Ordinary descending scans stop before collecting older segments'
            // holes once their visitor stops; global boundary holes stay visible.
            if stop_segment.is_none_or(|stop| segment >= stop) {
                push_with(&mut holes, hole, &control)?;
            }
        }
        sort_holes_with::<true, _, _>(&mut holes, &control, |h| h.from)?;
        dedup_holes_with::<true, _>(&mut holes, &control)?;
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
        let control = FnMutControl::new(check);
        control.check(0)?;
        let to = to.min(self.through);
        if from > to {
            return Ok(());
        }
        let mut next = Some(from);
        let mut stopped = false;
        self.walk(to, &control, &mut |_, _, record| {
            let Some(wanted) = next else {
                stopped = true;
                return Ok(false);
            };
            if record.last() < wanted {
                return Ok(true);
            }
            if record.first() > wanted {
                control.check(0)?;
                visit(LogCommit::Missing)?;
                control.check(0)?;
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
                        &control,
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
            control.check(0)?;
            if !visit(item)? || next.is_none_or(|n| n > to) {
                control.check(0)?;
                stopped = true;
                return Ok(false);
            }
            control.check(0)?;
            Ok(true)
        })?;
        if !stopped && next.is_some_and(|n| n <= to) {
            control.check(0)?;
            visit(LogCommit::Missing)?;
        }
        control.check(0)
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
        self.walk(self.through, &Unlimited, &mut |_, _, record| {
            first.get_or_insert(record.first());
            last = Some(record.last().min(self.through));
            Ok(true)
        })?;
        Ok((first, last))
    }
}

/// Where a read-only log's paged history read resumes (item 829).
#[derive(Default)]
pub(super) struct RoCursor {
    /// oldest first: the segment, the offset of the next record and the last commit
    /// read in that segment
    seg: usize,
    offset: Option<u64>,
    previous: Option<u64>,
    /// newest first: the offsets of the records left in the segment being read, and
    /// the segment read before it (`None` before the first)
    rev: Vec<RecordOffset>,
    rev_seg: Option<usize>,
}

impl RoCursor {
    /// What the cursor keeps allocated between pages.
    pub(super) fn kept_bytes(&self) -> u64 {
        (self.rev.capacity() * std::mem::size_of::<RecordOffset>()) as u64
    }
}

impl ReadOnlyLog {
    /// Read and decode the record at `offset` of `segment`, admitting its body and its
    /// decoded form before each is allocated.
    fn read_admitted<C: Control>(
        &self,
        segment: usize,
        offset: u64,
        control: &C,
        held: &super::pager::Held<'_, '_>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(body) =
            self.segments[segment].body_at_admitted(offset, control, &mut |b| held.record(b))?
        else {
            return Ok(None);
        };
        let Some(f) = footprint_with(&body, control)? else {
            return Ok(None);
        };
        held.record(body.capacity() as u64 + f.meta + f.terms)?;
        control.check(0)?;
        Ok(Some(body))
    }

    /// One page of a paged history read of commits `from` through `to`: the records
    /// whose changes `visit` takes, resuming where `cur` stopped, until `visit` stops
    /// or the page holds `budget` changes before a record is read or a segment is
    /// walked. A commit is one record (see `append_pending`), so stopping before a
    /// record never splits a commit. Returns whether the range was read to its end.
    /// The prefix of each segment that is read follows the rules of
    /// [`ReadOnlyLog::walk`]. Newest first, the offsets of one segment's records are
    /// kept, admitted as they grow, and read back from the newest.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn page(
        &self,
        cur: &mut RoCursor,
        from: u64,
        to: u64,
        filter: &LogFilter,
        descending: bool,
        budget: usize,
        check: &mut dyn FnMut() -> Result<()>,
        held: &super::pager::Held<'_, '_>,
        visit: &mut Visit<'_>,
    ) -> Result<bool> {
        let control = FnMutControl::new(check);
        control.check(0)?;
        let to = to.min(self.through);
        if from > to {
            return Ok(true);
        }
        let damaged = || Error::Corrupt("captured history record is no longer valid".into());
        if !descending {
            while cur.seg < self.segments.len() {
                control.check(0)?;
                if self.segments[cur.seg].first > to {
                    return Ok(true);
                }
                let mut offset = cur.offset.unwrap_or(SEG_HEADER as u64);
                loop {
                    if held.len.get() >= budget {
                        cur.offset = Some(offset);
                        return Ok(false);
                    }
                    let Some(body) = self.read_admitted(cur.seg, offset, &control, held)? else {
                        break;
                    };
                    let Some(record) = decode_view_with(&body, &control, true)? else {
                        break;
                    };
                    if cur
                        .previous
                        .is_some_and(|last: u64| last.checked_add(1) != Some(record.first()))
                        || record.first() > self.through
                    {
                        break;
                    }
                    if record.first() > to {
                        drop(record);
                        drop(body);
                        held.record(0)?;
                        return Ok(true);
                    }
                    if record.last() >= from
                        && matches!(record, RecordView::Changes { .. })
                        && !record.emit(filter, &control, visit)?
                    {
                        cur.offset = Some(offset);
                        return Ok(false);
                    }
                    let covered_to = record.last() >= to;
                    cur.previous = Some(record.last());
                    offset = offset
                        .checked_add(8 + body.len() as u64)
                        .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
                    drop(record);
                    drop(body);
                    held.record(0)?;
                    if covered_to {
                        return Ok(true);
                    }
                }
                held.record(0)?;
                cur.seg += 1;
                cur.offset = None;
                cur.previous = None;
            }
            return Ok(true);
        }
        loop {
            if let Some(item) = cur.rev.pop() {
                if held.len.get() >= budget {
                    cur.rev.push(item);
                    return Ok(false);
                }
                let body = self
                    .read_admitted(item.segment, item.offset, &control, held)?
                    .ok_or_else(damaged)?;
                let record = decode_view_with(&body, &control, true)?.ok_or_else(damaged)?;
                let more = record.emit(filter, &control, visit)?;
                drop(record);
                drop(body);
                held.record(0)?;
                if !more {
                    // a change of this record may be left: read it again next time
                    cur.rev.push(item);
                    return Ok(false);
                }
                continue;
            }
            let next = cur.rev_seg.unwrap_or(self.segments.len());
            if next == 0 {
                return Ok(true);
            }
            // a full page stops before it walks the next segment
            if held.len.get() >= budget {
                return Ok(false);
            }
            control.check(0)?;
            let segment = next - 1;
            cur.rev_seg = Some(segment);
            if self.segments[segment].first > to {
                continue;
            }
            let mut offset = SEG_HEADER as u64;
            let mut previous = None;
            loop {
                let Some(body) = self.read_admitted(segment, offset, &control, held)? else {
                    break;
                };
                let Some(record) = decode_view_with(&body, &control, true)? else {
                    break;
                };
                if previous.is_some_and(|last: u64| last.checked_add(1) != Some(record.first()))
                    || record.first() > to
                {
                    drop(record);
                    drop(body);
                    break;
                }
                if record.last() >= from && matches!(record, RecordView::Changes { .. }) {
                    if cur.rev.len() == cur.rev.capacity() {
                        // a reallocation holds the old offsets beside the new ones
                        let size = std::mem::size_of::<RecordOffset>() as u64;
                        let old = cur.rev.capacity() as u64 * size;
                        let more = cur.rev.capacity().max(16);
                        let new = old + more as u64 * size;
                        held.kept(old + new)?;
                        cur.rev.try_reserve_exact(more).map_err(|_| {
                            Error::HistoryUnsupported(
                                "history offset allocation was refused".into(),
                            )
                        })?;
                        held.kept(new)?;
                    }
                    cur.rev.push(RecordOffset { segment, offset });
                }
                let covered_to = record.last() >= to;
                previous = Some(record.last());
                offset = offset
                    .checked_add(8 + body.len() as u64)
                    .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
                drop(record);
                drop(body);
                held.record(0)?;
                if covered_to {
                    break;
                }
            }
            held.record(0)?;
        }
    }
}
