//! Read-only streaming committed-prefix checks. These records confer no authority.
use super::*;
use crate::store::cold::ColdBudget;

/// The same durability evidence as `wal_torn_from`, without retaining the log or
/// materializing any data transaction. The caller owns this exact file descriptor
/// and its captured extent; a path replacement cannot select another log.
fn durable_after<R: Read + Seek>(
    reader: &mut BufReader<R>,
    extent: u64,
    at: usize,
    seq: u64,
    framing: Framing,
    budget: &ColdBudget<'_>,
) -> Result<bool> {
    budget.check()?;
    if let Framing::Strict { from } = framing
        && at < from
    {
        return Ok(true);
    }
    let resume = reader.stream_position()?;
    let result = (|| {
        let start = at
            .checked_add(1)
            .and_then(|n| n.checked_mul(WAL_REC))
            .ok_or_else(|| Error::Corrupt("WAL record offset overflow".into()))?
            as u64;
        reader.seek(SeekFrom::Start(start))?;
        let mut record = [0; WAL_REC];
        let mut crc = flate2::Crc::new();
        for index in at.saturating_add(1)..(extent / WAL_REC as u64) as usize {
            budget.key_step("WAL durability record", index)?;
            reader.read_exact(&mut record)?;
            match record[0] {
                WAL_INSERT | WAL_DELETE | WAL_CONFIGURATION => crc.update(&record),
                WAL_COMMIT => {
                    match commit::open_wal_commit_with_crc(&record, crc) {
                        Some(Ok((s, ..)))
                            if s.checked_sub(1 + commit::wal_commit_unfenced(&record))
                                .is_some_and(|durable| durable >= seq) =>
                        {
                            return Ok(true);
                        }
                        None if !framing.strict_at(index) => return Ok(true),
                        _ => {}
                    }
                    crc = flate2::Crc::new();
                }
                _ => crc = flate2::Crc::new(),
            }
        }
        budget.check()?;
        Ok(false)
    })();
    // Restore even after refusal or cancellation; this never writes or repairs.
    let restore = reader.seek(SeekFrom::Start(resume));
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            restore?;
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(clippy::too_many_arguments)]
    fn prove_prefix(
        reader: &mut BufReader<File>,
        extent: u64,
        base: u64,
        target: u64,
        fold_legacy: bool,
        strict_from: Option<u64>,
        dvocab_len: u64,
        annotated: Option<&[u64]>,
        path: &Path,
        budget: &ColdBudget<'_>,
    ) -> Result<ProvenWal> {
        prove_prefix_with(
            reader,
            extent,
            base,
            target,
            fold_legacy,
            strict_from,
            dvocab_len,
            annotated,
            path,
            budget,
            &mut |_| Ok(()),
        )
    }

    use super::*;
    use crate::history::{HistoryOptions, ReadLimit};

    fn data(value: u8) -> [u8; WAL_REC] {
        let mut out = [value; WAL_REC];
        out[0] = WAL_INSERT;
        out
    }
    fn commit_record(seq: u64, data: &[u8], unfenced: u8) -> [u8; WAL_REC] {
        let mut out = [0; WAL_REC];
        out[0] = WAL_COMMIT;
        commit::seal_wal_commit_unfenced(&mut out, seq, 100, CommitKind::Update, 0, unfenced, data);
        out
    }

    #[test]
    fn normal_record_observer_excludes_actual_durability_lookahead_rereads() {
        struct Counted {
            cursor: std::io::Cursor<Vec<u8>>,
            read_bytes: usize,
        }
        impl Read for Counted {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = self.cursor.read(output)?;
                self.read_bytes += count;
                Ok(count)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, seek: SeekFrom) -> std::io::Result<u64> {
                self.cursor.seek(seek)
            }
        }
        let mut wire = header_record().to_vec();
        for seq in [1, 2] {
            let mut data = [0; WAL_REC];
            data[0] = WAL_INSERT;
            wire.extend_from_slice(&data);
            let mut commit = [0; WAL_REC];
            commit[0] = WAL_COMMIT;
            commit::seal_wal_commit_unfenced(
                &mut commit,
                seq,
                100,
                CommitKind::Update,
                if seq == 1 {
                    commit::WAL_FLAG_ANNOTATED
                } else {
                    0
                },
                0,
                &data,
            );
            wire.extend_from_slice(&commit);
        }
        let options = HistoryOptions::default();
        let budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })
        .unwrap();
        let mut reader = BufReader::with_capacity(
            WAL_REC,
            Counted {
                cursor: std::io::Cursor::new(wire.clone()),
                read_bytes: 0,
            },
        );
        let mut observed = Vec::new();
        let mut offsets = Vec::new();
        let proof = prove_prefix_recording(
            &mut reader,
            wire.len() as u64,
            0,
            2,
            false,
            None,
            u64::MAX,
            Some(&[]),
            Path::new("record-observer"),
            &budget,
            &mut |_| Ok(()),
            &mut |offset, record| {
                offsets.push(offset);
                observed.extend_from_slice(record);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(proof.head.seq, 2);
        assert_eq!(proof.head.offset, wire.len() as u64);
        assert!(
            reader.get_ref().read_bytes > wire.len(),
            "missing middle membership must actually cause lookahead"
        );
        assert_eq!(
            observed, wire,
            "only ordinary parser records enter the digest"
        );
        assert_eq!(
            offsets,
            (0..5)
                .map(|index| index * WAL_REC as u64)
                .collect::<Vec<_>>()
        );
        let mut unchanged = BufReader::with_capacity(WAL_REC, std::io::Cursor::new(&wire));
        let old = prove_prefix_with(
            &mut unchanged,
            wire.len() as u64,
            0,
            2,
            false,
            None,
            u64::MAX,
            Some(&[]),
            Path::new("record-observer"),
            &budget,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(old.head.seq, proof.head.seq);
        assert_eq!(old.head.offset, proof.head.offset);
    }

    #[test]
    fn normal_record_observer_refusal_never_returns_a_prefix_proof() {
        let mut wire = header_record().to_vec();
        wire.extend_from_slice(&data(1));
        let options = HistoryOptions::default();
        let budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })
        .unwrap();
        let mut reader = BufReader::new(std::io::Cursor::new(&wire));
        let mut observed = 0;
        let result = prove_prefix_recording(
            &mut reader,
            wire.len() as u64,
            0,
            0,
            false,
            None,
            u64::MAX,
            None,
            Path::new("record-observer"),
            &budget,
            &mut |_| Ok(()),
            &mut |offset, _| {
                if offset > 0 {
                    return Err(Error::Cancelled);
                }
                observed += 1;
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(observed, 1);
    }

    #[test]
    fn committed_prefix_refuses_durable_damage_but_keeps_exact_unsynced_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let options = HistoryOptions::default();
        for unfenced in [0, 1] {
            let d = data(0);
            let mut first = commit_record(11, &d, 0);
            first[27] = commit::WAL_FLAG_ANNOTATED;
            commit::seal_wal_commit_unfenced(
                &mut first,
                11,
                100,
                CommitKind::Update,
                commit::WAL_FLAG_ANNOTATED,
                0,
                &d,
            );
            let second = commit_record(12, &d, unfenced);
            let wire: Vec<_> = [header_record(), d, first, d, second]
                .into_iter()
                .flatten()
                .collect();
            std::fs::write(&path, &wire).unwrap();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(4096),
                held: 0,
            })
            .unwrap();
            let loaded = budget.buffered(&path).unwrap();
            let mut reader = loaded.value;
            // The later fenced commit proves a missing annotation was external damage;
            // an unfenced suffix has no such proof and stops before the first commit.
            let proof = prove_prefix(
                &mut reader,
                wire.len() as u64,
                10,
                11,
                false,
                None,
                u64::MAX,
                Some(&[]),
                &path,
                &budget,
            )
            .unwrap();
            assert_eq!(proof.head.seq, if unfenced == 0 { 12 } else { 10 });
            assert_eq!(proof.selected.is_some(), unfenced == 0);
            assert_eq!(std::fs::read(&path).unwrap(), wire);
            let mut damaged = wire.clone();
            damaged[WAL_REC + 1] ^= 1;
            std::fs::write(&path, &damaged).unwrap();
            let mut reader = BufReader::new(File::open(&path).unwrap());
            let result = prove_prefix(
                &mut reader,
                damaged.len() as u64,
                10,
                11,
                false,
                None,
                u64::MAX,
                None,
                &path,
                &budget,
            );
            if unfenced == 0 {
                assert!(matches!(result, Err(Error::Corrupt(_))));
            } else {
                assert_eq!(result.unwrap().head.seq, 10);
            }
            assert_eq!(std::fs::read(&path).unwrap(), damaged);
        }
    }

    #[test]
    fn durable_boundary_refuses_eof_and_partial_record_truncation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wal.log");
        let data = data(0);
        let wire: Vec<u8> = [
            data,
            commit_record(11, &data, 0),
            data,
            commit_record(12, &data, 0),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(wire.len(), 132);
        let options = HistoryOptions::default();
        for end in [0, 33, 66, 67, 98, 99, 131, 132] {
            let bytes = &wire[..end];
            std::fs::write(&path, bytes).unwrap();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(4096),
                held: 0,
            })
            .unwrap();
            let loaded = budget.buffered(&path).unwrap();
            let mut reader = loaded.value;
            let result = prove_prefix(
                &mut reader,
                end as u64,
                10,
                11,
                false,
                Some(132),
                u64::MAX,
                None,
                &path,
                &budget,
            );
            if end < 132 {
                assert!(
                    matches!(result, Err(Error::Corrupt(_))),
                    "accepted durable truncation at{end}: {result:?}"
                );
            } else {
                let result = result.unwrap();
                assert_eq!(result.head.seq, 12);
                assert_eq!(result.selected.unwrap().seq, 11);
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        // A complete first transaction is a valid prefix when only those bytes
        // were recorded durable, even if a later transaction has not reached disk.
        std::fs::write(&path, &wire[..66]).unwrap();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut reader = budget.buffered(&path).unwrap().value;
        assert_eq!(
            prove_prefix(
                &mut reader,
                66,
                10,
                11,
                false,
                Some(66),
                u64::MAX,
                None,
                &path,
                &budget
            )
            .unwrap()
            .head
            .seq,
            11
        );
        let mut headed = header_record().to_vec();
        headed.extend_from_slice(&wire);
        std::fs::write(&path, &headed).unwrap();
        let mut reader = budget.buffered(&path).unwrap().value;
        // A real header supersedes a stale headerless marker, exactly as recovery.
        assert_eq!(
            prove_prefix(
                &mut reader,
                headed.len() as u64,
                10,
                11,
                false,
                Some(3300),
                u64::MAX,
                None,
                &path,
                &budget
            )
            .unwrap()
            .head
            .seq,
            12
        );
        assert_eq!(std::fs::read(&path).unwrap(), headed);
    }

    #[test]
    fn physical_observations_distinguish_folded_sequences_and_skip_the_header() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("wal.log");
        let options = HistoryOptions::default();
        for (headed, modern, folding) in [
            (true, true, false),
            (false, true, false),
            (false, false, true),
            (false, false, false),
        ] {
            let mut wire = if headed {
                header_record().to_vec()
            } else {
                Vec::new()
            };
            let data = data(0);
            for seq in [11, 12] {
                wire.extend_from_slice(&data);
                let record = if modern {
                    commit_record(seq, &data, 0)
                } else {
                    let mut record = [0; WAL_REC];
                    record[0] = WAL_COMMIT;
                    record
                };
                wire.extend_from_slice(&record);
            }
            std::fs::write(&path, &wire).unwrap();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(4096),
                held: 0,
            })
            .unwrap();
            let mut reader = budget.buffered(&path).unwrap().value;
            let mut observed = Vec::new();
            let proof = prove_prefix_with(
                &mut reader,
                wire.len() as u64,
                10,
                u64::MAX,
                folding,
                None,
                u64::MAX,
                None,
                &path,
                &budget,
                &mut |record| {
                    observed.push((record.point.seq, record.start_offset, record.point.offset));
                    Ok(())
                },
            )
            .unwrap();
            let start = if headed { WAL_REC as u64 } else { 0 };
            let first_seq = if folding { 10 } else { 11 };
            let last_seq = if folding { 10 } else { 12 };
            assert_eq!(
                observed,
                [
                    (first_seq, start, start + 2 * WAL_REC as u64),
                    (
                        last_seq,
                        start + 2 * WAL_REC as u64,
                        start + 4 * WAL_REC as u64
                    ),
                ],
                "headed={headed} modern={modern} folding={folding}"
            );
            assert_eq!(proof.head.offset, wire.len() as u64);
            assert_eq!(std::fs::read(&path).unwrap(), wire);
        }
    }

    #[test]
    fn scalar_commit_observations_are_controlled_and_provisional_until_final_proof() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wal.log");
        let data = data(0);
        let wire: Vec<u8> = [
            data,
            commit_record(11, &data, 0),
            data,
            commit_record(12, &data, 0),
        ]
        .into_iter()
        .flatten()
        .collect();
        std::fs::write(&path, &wire).unwrap();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut reader = budget.buffered(&path).unwrap().value;
        let mut observed = Vec::new();
        let proof = prove_prefix_with(
            &mut reader,
            wire.len() as u64,
            10,
            11,
            false,
            Some(132),
            u64::MAX,
            None,
            &path,
            &budget,
            &mut |record| {
                observed.push((
                    record.point.seq,
                    record.start_offset,
                    record.point.offset,
                    record.metadata,
                ));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(proof.head.seq, 12);
        assert_eq!(
            observed,
            [
                (11, 0, 66, Some((100, CommitKind::Update, 0))),
                (12, 66, 132, Some((100, CommitKind::Update, 0)))
            ]
        );
        std::fs::write(&path, &wire[..66]).unwrap();
        let mut reader = budget.buffered(&path).unwrap().value;
        observed.clear();
        let result = prove_prefix_with(
            &mut reader,
            66,
            10,
            11,
            false,
            Some(132),
            u64::MAX,
            None,
            &path,
            &budget,
            &mut |record| {
                observed.push((
                    record.point.seq,
                    record.start_offset,
                    record.point.offset,
                    record.metadata,
                ));
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::Corrupt(_))));
        assert_eq!(
            observed.len(),
            1,
            "observations must be discarded after the final refusal"
        );
        std::fs::write(&path, &wire).unwrap();
        let mut reader = budget.buffered(&path).unwrap().value;
        let mut visits = 0;
        assert!(matches!(
            prove_prefix_with(
                &mut reader,
                wire.len() as u64,
                10,
                11,
                false,
                Some(132),
                u64::MAX,
                None,
                &path,
                &budget,
                &mut |_| {
                    visits += 1;
                    Err(Error::Cancelled)
                }
            ),
            Err(Error::Cancelled)
        ));
        assert_eq!(visits, 1);
        assert_eq!(std::fs::read(&path).unwrap(), wire);
    }

    #[test]
    fn streaming_durability_matches_existing_classifier_for_damage_and_fences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let options = HistoryOptions::default();
        for unfenced in [0, 1, 2, 255] {
            let mut records = vec![header_record(), data(1)];
            records.push(commit_record(11, &records[1], unfenced));
            records.push(data(2));
            records.push(commit_record(12, &records[3], unfenced));
            records.push(data(3));
            records.push(commit_record(13, &records[5], unfenced));
            let original: Vec<_> = records.iter().flatten().copied().collect();
            for damage in 0..records.len() {
                for byte in [0, 9, 26, 28, 32] {
                    let mut wire = original.clone();
                    wire[damage * WAL_REC + byte] ^= 0x7f;
                    std::fs::write(&path, &wire).unwrap();
                    let mut budget = ColdBudget::new(ReadLimit {
                        options: &options,
                        memory: Some(4096),
                        held: 0,
                    })
                    .unwrap();
                    let loaded = budget.buffered(&path).unwrap();
                    let mut reader = loaded.value;
                    reader.seek(SeekFrom::Start(3)).unwrap();
                    for framing in [
                        Framing::Mixed,
                        Framing::Strict { from: 1 },
                        Framing::Strict { from: 4 },
                    ] {
                        for seq in [10, 11, 12, 13, 14] {
                            let actual = durable_after(
                                &mut reader,
                                wire.len() as u64,
                                damage,
                                seq,
                                framing,
                                &budget,
                            )
                            .unwrap();
                            assert_eq!(
                                actual,
                                !wal_torn_from(&wire, damage, seq, framing),
                                "damage={damage} byte={byte} seq={seq} framing={framing:?}"
                            );
                            assert_eq!(reader.stream_position().unwrap(), 3);
                        }
                    }
                    assert_eq!(std::fs::read(&path).unwrap(), wire);
                }
            }
        }
    }
}

/// Capture only committed offsets and sequence numbers. Reconstructing a selected
/// state's counts remains the selected replay's job; this never constructs head.
#[derive(Debug)]
pub(crate) struct ProvenWal {
    pub(crate) head: WalPoint,
    pub(crate) selected: Option<WalPoint>,
}

/// Provisional metadata of one physically decoded commit. Observations may be
/// used only after the complete probe succeeds, including its final durable bound.
/// The origin reader resolves timestamps/counts; this is not admission or identity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WalCommitObservation {
    /// Actual first byte of this transaction, after a header or the prior commit.
    /// This framing position is not itself authenticated imported authority.
    pub(crate) start_offset: u64,
    pub(crate) point: WalPoint,
    pub(crate) metadata: Option<(i64, CommitKind, u8)>,
    pub(crate) configuration: Option<crate::evidence::Sha256Digest>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prove_prefix_with<R: Read + Seek>(
    reader: &mut BufReader<R>,
    extent: u64,
    base: u64,
    target: u64,
    fold_legacy: bool,
    strict_from: Option<u64>,
    dvocab_len: u64,
    annotated: Option<&[u64]>,
    path: &Path,
    budget: &ColdBudget<'_>,
    visit: &mut dyn FnMut(WalCommitObservation) -> Result<()>,
) -> Result<ProvenWal> {
    prove_prefix_recording(
        reader,
        extent,
        base,
        target,
        fold_legacy,
        strict_from,
        dvocab_len,
        annotated,
        path,
        budget,
        visit,
        &mut |_, _| Ok(()),
    )
}

/// Observe the fixed records used by the ordinary parser, never its durability
/// lookahead/rereads. Both record bytes and commit observations remain provisional
/// until the complete prefix proof succeeds. This grants no source identity.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prove_prefix_recording<
    R: Read + Seek,
    F: FnMut(u64, &[u8; WAL_REC]) -> Result<()>,
>(
    reader: &mut BufReader<R>,
    extent: u64,
    base: u64,
    target: u64,
    fold_legacy: bool,
    strict_from: Option<u64>,
    dvocab_len: u64,
    annotated: Option<&[u64]>,
    path: &Path,
    budget: &ColdBudget<'_>,
    visit: &mut dyn FnMut(WalCommitObservation) -> Result<()>,
    record_used: &mut F,
) -> Result<ProvenWal> {
    prove_prefix_bounded(
        reader,
        extent,
        base,
        target,
        fold_legacy,
        strict_from,
        dvocab_len,
        annotated,
        path,
        budget,
        visit,
        record_used,
        false,
    )
}

/// Prove the log only through commit `target`, as a past-state read of `target`
/// replays it. Damage to a later commit, durable or not, belongs to the reads of
/// that commit, so it cannot refuse an earlier one. Observations end at `target`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prove_prefix_through<R: Read + Seek>(
    reader: &mut BufReader<R>,
    extent: u64,
    base: u64,
    target: u64,
    fold_legacy: bool,
    strict_from: Option<u64>,
    dvocab_len: u64,
    annotated: Option<&[u64]>,
    path: &Path,
    budget: &ColdBudget<'_>,
    visit: &mut dyn FnMut(WalCommitObservation) -> Result<()>,
) -> Result<ProvenWal> {
    prove_prefix_bounded(
        reader,
        extent,
        base,
        target,
        fold_legacy,
        strict_from,
        dvocab_len,
        annotated,
        path,
        budget,
        visit,
        &mut |_, _| Ok(()),
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn prove_prefix_bounded<R: Read + Seek, F: FnMut(u64, &[u8; WAL_REC]) -> Result<()>>(
    reader: &mut BufReader<R>,
    extent: u64,
    base: u64,
    target: u64,
    fold_legacy: bool,
    strict_from: Option<u64>,
    dvocab_len: u64,
    annotated: Option<&[u64]>,
    path: &Path,
    budget: &ColdBudget<'_>,
    visit: &mut dyn FnMut(WalCommitObservation) -> Result<()>,
    record_used: &mut F,
    stop_at_target: bool,
) -> Result<ProvenWal> {
    check_strict_from(path, strict_from)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut head = WalPoint {
        seq: base,
        offset: 0,
        folding: fold_legacy,
    };
    let mut selected = (target == base).then_some(head);
    if stop_at_target && selected.is_some() && !head.folding {
        budget.check()?;
        return Ok(ProvenWal { head, selected });
    }
    let mut framing = Framing::of(strict_from);
    let mut crc = flate2::Crc::new();
    let mut needed = 0;
    let mut seen_modern = false;
    let mut header = false;
    let mut wal_version = 0;
    let mut configuration = ConfigurationRecords::default();
    let mut record = [0; WAL_REC];
    let corrupt = || Error::Corrupt(format!("{}: invalid committed WAL prefix", path.display()));
    for index in 0..(extent / WAL_REC as u64) as usize {
        budget.key_step("WAL prefix record", index)?;
        reader.read_exact(&mut record)?;
        match record[0] {
            WAL_HEADER if index == 0 => {
                header = true;
                wal_version = header_version(&record, path)?;
                if wal_version >= 1 {
                    framing = Framing::Strict { from: 1 };
                }
                head.offset = WAL_REC as u64;
            }
            WAL_INSERT | WAL_DELETE => {
                configuration.data(&record);
                crc.update(&record);
                let quad = std::array::from_fn(|j| {
                    Id(u64::from_le_bytes(
                        record[1 + j * 8..9 + j * 8].try_into().unwrap(),
                    ))
                });
                needed = needed.max(delta_ids_end(std::iter::once(&quad)));
            }
            WAL_CONFIGURATION => {
                configuration.data(&record);
                crc.update(&record);
            }
            WAL_COMMIT => {
                let meta = commit::open_wal_commit_with_crc(&record, crc);
                let next = head.seq.checked_add(1).ok_or_else(corrupt)?;
                let invalid = matches!(meta, Some(Err(())))
                    || (meta.is_none() && framing.strict_at(index))
                    || needed > dvocab_len;
                if invalid {
                    if durable_after(reader, extent, index, next, framing, budget)? {
                        return Err(corrupt());
                    }
                    break;
                }
                let configuration_digest = configuration.finish(
                    wal_version,
                    match &meta {
                        Some(Ok((_, _, _, flags))) => *flags,
                        _ => 0,
                    },
                )?;
                if let (Some(seqs), Some(Ok((seq, _, _, flags)))) = (annotated, &meta)
                    && flags & commit::WAL_FLAG_ANNOTATED != 0
                    && seqs.binary_search(seq).is_err()
                    && !durable_after(reader, extent, index, next, framing, budget)?
                {
                    break;
                }
                match meta {
                    Some(Ok((seq, ..))) => {
                        if seq != next {
                            return Err(corrupt());
                        }
                        head.seq = seq;
                        seen_modern = true;
                    }
                    _ if fold_legacy && !seen_modern => {}
                    _ => head.seq = next,
                }
                let start_offset = head.offset;
                head.offset = (index as u64 + 1) * WAL_REC as u64;
                head.folding = fold_legacy && !seen_modern;
                if head.seq == target {
                    selected = Some(head);
                }
                budget.check()?;
                visit(WalCommitObservation {
                    start_offset,
                    point: head,
                    configuration: configuration_digest,
                    metadata: match meta {
                        Some(Ok((_, timestamp, kind, flags))) => Some((timestamp, kind, flags)),
                        _ => None,
                    },
                })?;
                budget.check()?;
                // Folded legacy commits share one number, so a folding prefix
                // continues to the first commit that ends the fold.
                if stop_at_target && selected.is_some() && !head.folding {
                    return Ok(ProvenWal { head, selected });
                }
                crc = flate2::Crc::new();
                needed = 0;
                configuration = ConfigurationRecords::default();
            }
            _ => {
                let next = head.seq.checked_add(1).ok_or_else(corrupt)?;
                if durable_after(reader, extent, index, next, framing, budget)? {
                    return Err(corrupt());
                }
                break;
            }
        }
        record_used(index as u64 * WAL_REC as u64, &record)?;
    }
    // A headerless generation recorded these bytes durable before raising its
    // framing boundary. EOF and partial records cannot erase that evidence. A
    // real header supersedes this legacy marker, as in ordinary recovery.
    if let Some(boundary) = strict_from
        && !header
        && head.offset < boundary
    {
        return Err(corrupt());
    }
    budget.check()?;
    Ok(ProvenWal { head, selected })
}
