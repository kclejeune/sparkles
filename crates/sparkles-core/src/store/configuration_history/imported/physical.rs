//! Streaming byte and transaction-boundary checks. These private checks do not
//! establish a source family, publication, physical owner or imported authority.

use super::*;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Seek, SeekFrom};

/// The owning import context admits the actual interval storage before lending
/// it here. Neither checker copies it or allocates another interval container.
pub(super) struct Span {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) sha256: Sha256Digest,
    pub(super) first_seq: u64,
    pub(super) last_seq: u64,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "imported WAL physical proof differs",
    )
}

fn validate(spans: &[Span], extent: u64) -> io::Result<()> {
    if spans.len() > MAX_MEMBERS {
        return Err(invalid());
    }
    let mut previous_end = 0;
    for span in spans {
        if span.start < previous_end
            || span.start >= span.end
            || span.end > extent
            || !span.start.is_multiple_of(WAL_REC as u64)
            || !span.end.is_multiple_of(WAL_REC as u64)
            || span.first_seq > span.last_seq
        {
            return Err(invalid());
        }
        previous_end = span.end;
    }
    Ok(())
}

/// Place below the existing BufReader, so the digest consumes the same bytes
/// which reach the authoritative parser. Only one hash state lives at a time.
pub(super) struct SpanReader<'a, R> {
    inner: R,
    spans: &'a [Span],
    extent: u64,
    position: u64,
    index: usize,
    covered: u64,
    digest: Sha256,
}

impl<'a, R: Read + Seek> SpanReader<'a, R> {
    pub(super) fn new(mut inner: R, spans: &'a [Span], extent: u64) -> io::Result<Self> {
        validate(spans, extent)?;
        inner.seek(SeekFrom::Start(0))?;
        Ok(Self {
            inner,
            spans,
            extent,
            position: 0,
            index: 0,
            covered: 0,
            digest: Sha256::new(),
        })
    }

    pub(super) fn finish(self) -> io::Result<R> {
        if self.index != self.spans.len() || self.covered != 0 {
            return Err(invalid());
        }
        Ok(self.inner)
    }
}

impl<R: Read + Seek> Read for SpanReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let limit = (self.extent.saturating_sub(self.position)).min(bytes.len() as u64) as usize;
        let count = self.inner.read(&mut bytes[..limit])?;
        let end = self
            .position
            .checked_add(count as u64)
            .ok_or_else(invalid)?;
        while let Some(span) = self.spans.get(self.index) {
            if span.start >= end {
                break;
            }
            let start = self.position.max(span.start);
            let stop = end.min(span.end);
            if start > stop || start != span.start + self.covered {
                return Err(invalid());
            }
            self.digest
                .update(&bytes[(start - self.position) as usize..(stop - self.position) as usize]);
            self.covered += stop - start;
            if stop != span.end {
                break;
            }
            let digest: [u8; 32] = std::mem::replace(&mut self.digest, Sha256::new())
                .finalize()
                .into();
            if &digest != span.sha256.as_bytes() {
                return Err(invalid());
            }
            self.index += 1;
            self.covered = 0;
        }
        self.position = end;
        Ok(count)
    }
}

/// Hash only normal authoritative parser observations, never speculative
/// durability lookahead. Fixed hash state borrows the actual admitted intervals.
struct RecordProof<'a> {
    spans: &'a [Span],
    extent: u64,
    position: u64,
    index: usize,
    covered: u64,
    digest: Sha256,
}
impl<'a> RecordProof<'a> {
    fn new(spans: &'a [Span], extent: u64) -> io::Result<Self> {
        validate(spans, extent)?;
        Ok(Self {
            spans,
            extent,
            position: 0,
            index: 0,
            covered: 0,
            digest: Sha256::new(),
        })
    }

    fn observe(&mut self, offset: u64, record: &[u8; WAL_REC]) -> Result<()> {
        let end = offset.checked_add(WAL_REC as u64).ok_or_else(invalid)?;
        if offset != self.position || end > self.extent {
            return Err(invalid().into());
        }
        if let Some(span) = self.spans.get(self.index) {
            if offset >= span.start {
                if offset != span.start + self.covered || end > span.end {
                    return Err(invalid().into());
                }
                self.digest.update(record);
                self.covered += WAL_REC as u64;
                if end == span.end {
                    let digest: [u8; 32] = std::mem::replace(&mut self.digest, Sha256::new())
                        .finalize()
                        .into();
                    if &digest != span.sha256.as_bytes() {
                        return Err(invalid().into());
                    }
                    self.index += 1;
                    self.covered = 0;
                }
            } else if end > span.start {
                return Err(invalid().into());
            }
        }
        self.position = end;
        Ok(())
    }

    fn finish(self) -> Result<()> {
        if self.index != self.spans.len() || self.covered != 0 {
            return Err(invalid().into());
        }
        Ok(())
    }
}

/// Physical prefix proof only. The caller must separately own/admit the actual
/// descriptor, span storage and source/base/publication assignment. No replay
/// authority follows from a scalar proof over an arbitrary reader.
#[allow(clippy::too_many_arguments)]
pub(super) fn prove_prefix<R: Read + Seek>(
    input: R,
    spans: &[Span],
    extent: u64,
    base: u64,
    target: u64,
    fold_legacy: bool,
    strict_from: Option<u64>,
    dvocab_len: u64,
    annotated: Option<&[u64]>,
    path: &Path,
    budget: &mut crate::store::cold::ColdBudget<'_>,
    visit: &mut dyn FnMut(wal::probe::WalCommitObservation) -> Result<()>,
) -> Result<(wal::probe::ProvenWal, R)> {
    let buffer = (budget.available() / 8).clamp(64, 64 << 10);
    let fixed = (std::mem::size_of::<RecordProof<'_>>()
        + std::mem::size_of::<FrameProof<'_>>()
        + std::mem::size_of::<std::io::BufReader<R>>()) as u64;
    let allowance = buffer
        .checked_add(fixed)
        .ok_or_else(|| budget.exhausted())?;
    budget.charge(allowance)?;
    let result = (|| {
        let mut frames = FrameProof::new(spans, extent)?;
        let mut records = RecordProof::new(spans, extent)?;
        let mut reader = std::io::BufReader::with_capacity(buffer as usize, input);
        let proof = wal::probe::prove_prefix_recording(
            &mut reader,
            extent,
            base,
            target,
            fold_legacy,
            strict_from,
            dvocab_len,
            annotated,
            path,
            budget,
            &mut |observation| {
                frames.observe(observation)?;
                visit(observation)
            },
            &mut |offset, record| records.observe(offset, record),
        )?;
        frames.finish()?;
        records.finish()?;
        let input = reader.into_inner();
        Ok((proof, input))
    })();
    budget.release(allowance);
    result
}

impl<R: Read + Seek> Seek for SpanReader<'_, R> {
    fn seek(&mut self, request: SeekFrom) -> io::Result<u64> {
        // stream_position uses Current(0), which must not discard buffered data
        // or make an already read byte count as a second import observation.
        if matches!(request, SeekFrom::Current(0)) {
            return Ok(self.position);
        }
        let target = match request {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.extent.checked_add_signed(offset),
        }
        .ok_or_else(invalid)?;
        // Ordinary lookahead after the last import remains seekable. A read of
        // an imported span must stay sequential; it cannot reuse an old digest.
        let lo = target.min(self.position);
        let hi = target.max(self.position);
        if target != self.position
            && self
                .spans
                .iter()
                .any(|span| lo < span.end && hi > span.start)
        {
            return Err(invalid());
        }
        let actual = self.inner.seek(SeekFrom::Start(target))?;
        if actual != target {
            return Err(invalid());
        }
        self.position = actual;
        Ok(actual)
    }
}

/// A separate observer checks actual transaction boundaries. Byte read-ahead
/// may complete hashes before these observations, so the states stay separate.
pub(super) struct FrameProof<'a> {
    spans: &'a [Span],
    index: usize,
    started: bool,
}

impl<'a> FrameProof<'a> {
    pub(super) fn new(spans: &'a [Span], extent: u64) -> io::Result<Self> {
        validate(spans, extent)?;
        Ok(Self {
            spans,
            index: 0,
            started: false,
        })
    }

    pub(super) fn observe(&mut self, observation: wal::probe::WalCommitObservation) -> Result<()> {
        let Some(span) = self.spans.get(self.index) else {
            return Ok(());
        };
        let start = observation.start_offset;
        let end = observation.point.offset;
        if end <= span.start {
            return Ok(());
        }
        if !self.started {
            // Only the original parser can emit start=WAL_REC for the first
            // transaction after its genuine initial header at offset0.
            if !(start == span.start || (span.start == 0 && start == WAL_REC as u64))
                || observation.point.seq != span.first_seq
            {
                return Err(managed_history::unavailable(
                    "imported WAL start is not a proven transaction boundary",
                ));
            }
            self.started = true;
        }
        if end > span.end {
            return Err(managed_history::unavailable(
                "imported WAL end splits a transaction",
            ));
        }
        if end == span.end {
            if observation.point.seq != span.last_seq {
                return Err(managed_history::unavailable(
                    "imported WAL final sequence differs",
                ));
            }
            self.index += 1;
            self.started = false;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<()> {
        if self.index != self.spans.len() || self.started {
            return Err(managed_history::unavailable(
                "imported WAL interval was not fully framed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::cold::ColdBudget;
    use std::io::{BufReader, Cursor};

    fn span(bytes: &[u8], start: usize, end: usize, first_seq: u64, last_seq: u64) -> Span {
        Span {
            start: start as u64,
            end: end as u64,
            sha256: Sha256Digest::of_bytes(&bytes[start..end], &crate::task::Control::none())
                .unwrap(),
            first_seq,
            last_seq,
        }
    }

    #[test]
    fn imported_actual_annotation_lookahead_preserves_the_same_durable_prefix() {
        struct Counted {
            cursor: Cursor<Vec<u8>>,
            bytes: usize,
        }
        impl Read for Counted {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let n = self.cursor.read(output)?;
                self.bytes += n;
                Ok(n)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, request: SeekFrom) -> io::Result<u64> {
                self.cursor.seek(request)
            }
        }
        let mut wire = wal::header_record().to_vec();
        let mut data = [0; WAL_REC];
        data[0] = WAL_INSERT;
        for seq in [1, 2] {
            wire.extend_from_slice(&data);
            let mut record = [0; WAL_REC];
            record[0] = WAL_COMMIT;
            commit::seal_wal_commit_unfenced(
                &mut record,
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
            wire.extend_from_slice(&record);
        }
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut baseline = BufReader::with_capacity(
            WAL_REC,
            Counted {
                cursor: Cursor::new(wire.clone()),
                bytes: 0,
            },
        );
        let accepted = wal::probe::prove_prefix_with(
            &mut baseline,
            wire.len() as u64,
            0,
            2,
            false,
            None,
            u64::MAX,
            Some(&[]),
            Path::new("annotation-lookahead"),
            &budget,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(accepted.head.seq, 2);
        assert_eq!(accepted.head.offset, wire.len() as u64);
        assert!(
            baseline.get_ref().bytes > wire.len(),
            "real durability reread must occur"
        );
        let spans = [span(&wire, 0, wire.len(), 1, 2)];
        let before = budget.used();
        let result = super::prove_prefix(
            Counted {
                cursor: Cursor::new(wire.clone()),
                bytes: 0,
            },
            &spans,
            wire.len() as u64,
            0,
            2,
            false,
            None,
            u64::MAX,
            Some(&[]),
            Path::new("annotation-lookahead"),
            &mut budget,
            &mut |_| Ok(()),
        );
        assert_eq!(budget.used(), before);
        assert!(
            result.is_ok(),
            "valid imported annotation lookahead must preserve the authoritative prefix: {:?}",
            result.as_ref().err()
        );
        let (imported, actual_reader) = result.unwrap();
        assert_eq!(imported.head.seq, accepted.head.seq);
        assert_eq!(imported.head.offset, accepted.head.offset);
        assert!(actual_reader.bytes > wire.len());
    }

    fn framed_wire(commits: u64, first_annotated: bool) -> Vec<u8> {
        let mut wire = wal::header_record().to_vec();
        let mut data = [0; WAL_REC];
        data[0] = WAL_INSERT;
        for seq in 1..=commits {
            wire.extend_from_slice(&data);
            let mut record = [0; WAL_REC];
            record[0] = WAL_COMMIT;
            commit::seal_wal_commit_unfenced(
                &mut record,
                seq,
                100,
                CommitKind::Update,
                if seq == 1 && first_annotated {
                    commit::WAL_FLAG_ANNOTATED
                } else {
                    0
                },
                0,
                &data,
            );
            wire.extend_from_slice(&record);
        }
        wire
    }

    #[test]
    fn imported_normal_record_digest_handles_disjoint_spans_and_native_suffix() {
        let wire = framed_wire(4, false);
        let spans = [
            span(&wire, 0, 3 * WAL_REC, 1, 1),
            span(&wire, 5 * WAL_REC, 7 * WAL_REC, 3, 3),
        ];
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let before = budget.used();
        let (proof, _) = super::prove_prefix(
            Cursor::new(&wire),
            &spans,
            wire.len() as u64,
            0,
            4,
            false,
            None,
            u64::MAX,
            None,
            Path::new("disjoint-imports"),
            &mut budget,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(proof.head.seq, 4);
        assert_eq!(proof.head.offset, wire.len() as u64);
        assert_eq!(budget.used(), before);
        let mut changed = wire.clone();
        changed[2 * WAL_REC - 1] ^= 1;
        let mut data = [0; WAL_REC];
        data.copy_from_slice(&changed[WAL_REC..2 * WAL_REC]);
        let record: &mut [u8; WAL_REC] =
            (&mut changed[2 * WAL_REC..3 * WAL_REC]).try_into().unwrap();
        commit::seal_wal_commit_unfenced(record, 1, 100, CommitKind::Update, 0, 0, &data);
        // The mutated prefix has a valid new CRC and remains an ordinary valid
        // WAL; only the original import digest must refuse it.
        let mut ordinary = BufReader::new(Cursor::new(&changed));
        let baseline = wal::probe::prove_prefix_with(
            &mut ordinary,
            changed.len() as u64,
            0,
            4,
            false,
            None,
            u64::MAX,
            None,
            Path::new("disjoint-imports"),
            &budget,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(baseline.head.seq, 4);
        assert!(
            super::prove_prefix(
                Cursor::new(&changed),
                &spans,
                changed.len() as u64,
                0,
                4,
                false,
                None,
                u64::MAX,
                None,
                Path::new("disjoint-imports"),
                &mut budget,
                &mut |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(budget.used(), before);
    }

    #[test]
    fn imported_normal_record_proof_requires_frames_and_actual_durability() {
        let wire = framed_wire(2, false);
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let before = budget.used();
        // The hash is genuine, but this interval contains data and ends before
        // that transaction's real commit; byte completion cannot authorize it.
        let spans = [span(&wire, WAL_REC, 2 * WAL_REC, 1, 1)];
        assert!(
            super::prove_prefix(
                Cursor::new(&wire),
                &spans,
                wire.len() as u64,
                0,
                2,
                false,
                None,
                u64::MAX,
                None,
                Path::new("split-import"),
                &mut budget,
                &mut |_| Ok(())
            )
            .is_err()
        );
        let truncated = framed_wire(1, true);
        let mut ordinary = BufReader::new(Cursor::new(&truncated));
        let proof = wal::probe::prove_prefix_with(
            &mut ordinary,
            truncated.len() as u64,
            0,
            0,
            false,
            None,
            u64::MAX,
            Some(&[]),
            Path::new("missing-durability"),
            &budget,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(proof.head.seq, 0);
        let spans = [span(&truncated, 0, truncated.len(), 1, 1)];
        assert!(
            super::prove_prefix(
                Cursor::new(&truncated),
                &spans,
                truncated.len() as u64,
                0,
                0,
                false,
                None,
                u64::MAX,
                Some(&[]),
                Path::new("missing-durability"),
                &mut budget,
                &mut |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(budget.used(), before);
    }

    #[test]
    fn imported_normal_record_proof_refuses_cancel_and_buffer_pressure_without_credit_leak() {
        struct Unread;
        impl Read for Unread {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("buffer must be admitted before reading");
            }
        }
        impl Seek for Unread {
            fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
                panic!("buffer must be admitted before seeking");
            }
        }
        let wire = framed_wire(2, false);
        let spans = [span(&wire, 0, wire.len(), 1, 2)];
        let options = HistoryOptions::default();
        let mut small = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            super::prove_prefix(
                Unread,
                &spans,
                wire.len() as u64,
                0,
                2,
                false,
                None,
                u64::MAX,
                None,
                Path::new("buffer-pressure"),
                &mut small,
                &mut |_| Ok(())
            ),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.used(), 0);
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut observed = 0;
        let result = super::prove_prefix(
            Cursor::new(&wire),
            &spans,
            wire.len() as u64,
            0,
            2,
            false,
            None,
            u64::MAX,
            None,
            Path::new("callback-refusal"),
            &mut budget,
            &mut |_| {
                observed += 1;
                Err(Error::Cancelled)
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(observed, 1);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn imported_normal_record_digest_refuses_duplicate_skip_and_incomplete_callbacks() {
        let wire = framed_wire(2, false);
        let spans = [span(&wire, 0, wire.len(), 1, 2)];
        let first: &[u8; WAL_REC] = wire[..WAL_REC].try_into().unwrap();
        let mut skipped = RecordProof::new(&spans, wire.len() as u64).unwrap();
        assert!(skipped.observe(WAL_REC as u64, first).is_err());
        let mut duplicate = RecordProof::new(&spans, wire.len() as u64).unwrap();
        duplicate.observe(0, first).unwrap();
        assert!(duplicate.observe(0, first).is_err());
        let mut incomplete = RecordProof::new(&spans, wire.len() as u64).unwrap();
        incomplete.observe(0, first).unwrap();
        assert!(incomplete.finish().is_err());
    }

    #[test]
    fn imported_bytes_stream_across_disjoint_spans_and_native_suffix() {
        let wire: Vec<_> = (0..WAL_REC * 8).map(|index| index as u8).collect();
        let spans = [
            span(&wire, 0, WAL_REC * 2, 11, 11),
            span(&wire, WAL_REC * 4, WAL_REC * 6, 14, 15),
        ];
        for chunk in [1, 13, WAL_REC, WAL_REC * 8] {
            let mut reader =
                SpanReader::new(Cursor::new(&wire), &spans, wire.len() as u64).unwrap();
            let mut scratch = vec![0; chunk];
            let mut observed = Vec::new();
            loop {
                let count = reader.read(&mut scratch).unwrap();
                if count == 0 {
                    break;
                }
                observed.extend_from_slice(&scratch[..count]);
            }
            reader.finish().unwrap();
            assert_eq!(observed, wire);
        }
    }

    #[test]
    fn imported_prefix_rewrite_refuses_even_when_appended_bytes_survive() {
        let original = vec![7; WAL_REC * 2];
        let spans = [span(&original, 0, original.len(), 11, 11)];
        let mut appended = original.clone();
        appended.extend_from_slice(&[9; WAL_REC * 2]);
        let mut reader =
            SpanReader::new(Cursor::new(&appended), &spans, appended.len() as u64).unwrap();
        reader.read_to_end(&mut Vec::new()).unwrap();
        reader.finish().unwrap();
        appended[3] ^= 1;
        let mut reader =
            SpanReader::new(Cursor::new(&appended), &spans, appended.len() as u64).unwrap();
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        assert!(reader.finish().is_err());
    }

    #[test]
    fn imported_reader_refuses_skips_rereads_and_incomplete_intervals() {
        let wire = vec![3; WAL_REC * 4];
        let spans = [span(&wire, WAL_REC, WAL_REC * 3, 11, 12)];
        let mut reader = SpanReader::new(Cursor::new(&wire), &spans, wire.len() as u64).unwrap();
        assert!(reader.seek(SeekFrom::Start((WAL_REC * 2) as u64)).is_err());
        assert_eq!(reader.stream_position().unwrap(), 0);
        let mut prefix = [0; WAL_REC * 2];
        reader.read_exact(&mut prefix).unwrap();
        assert_eq!(
            reader.seek(SeekFrom::Start((WAL_REC * 2) as u64)).unwrap(),
            (WAL_REC * 2) as u64
        );
        assert!(reader.seek(SeekFrom::Start(WAL_REC as u64)).is_err());
        assert!(reader.finish().is_err());
        let mut reader = SpanReader::new(Cursor::new(&wire), &spans, wire.len() as u64).unwrap();
        reader.read_to_end(&mut Vec::new()).unwrap();
        reader.seek(SeekFrom::Start((WAL_REC * 3) as u64)).unwrap();
        reader.read_to_end(&mut Vec::new()).unwrap();
        reader.finish().unwrap();
    }

    fn observation(start: u64, end: u64, seq: u64) -> wal::probe::WalCommitObservation {
        wal::probe::WalCommitObservation {
            start_offset: start,
            point: wal::WalPoint {
                seq,
                offset: end,
                folding: false,
            },
            metadata: None,
            configuration: None,
        }
    }

    #[test]
    fn imported_framing_refuses_aligned_midtransaction_and_wrong_sequence() {
        let wire = vec![0; WAL_REC * 4];
        for (start, end, first, last) in [
            (WAL_REC, WAL_REC * 4, 11, 12),
            (0, WAL_REC * 3, 11, 12),
            (0, WAL_REC * 4, 10, 12),
            (0, WAL_REC * 4, 11, 13),
        ] {
            let spans = [span(&wire, start, end, first, last)];
            let mut frames = FrameProof::new(&spans, wire.len() as u64).unwrap();
            let result = (|| {
                frames.observe(observation(0, (WAL_REC * 2) as u64, 11))?;
                frames.observe(observation((WAL_REC * 2) as u64, (WAL_REC * 4) as u64, 12))?;
                frames.finish()
            })();
            assert!(
                result.is_err(),
                "start={start},end={end},first={first},last={last}"
            );
        }
    }

    #[test]
    fn imported_stream_and_framing_use_the_actual_probe_for_headers_and_folded_commits() {
        for (headed, folded) in [(true, false), (false, true)] {
            let mut wire = if headed {
                wal::header_record().to_vec()
            } else {
                Vec::new()
            };
            let mut data = [0; WAL_REC];
            data[0] = WAL_INSERT;
            for seq in [11, 12] {
                wire.extend_from_slice(&data);
                let mut record = [0; WAL_REC];
                record[0] = WAL_COMMIT;
                if !folded {
                    commit::seal_wal_commit_unfenced(
                        &mut record,
                        seq,
                        100,
                        CommitKind::Update,
                        0,
                        0,
                        &data,
                    );
                }
                wire.extend_from_slice(&record);
            }
            let spans = [span(
                &wire,
                0,
                wire.len(),
                if folded { 10 } else { 11 },
                if folded { 10 } else { 12 },
            )];
            let options = HistoryOptions::default();
            let budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(4096),
                held: 0,
            })
            .unwrap();
            let mut reader = BufReader::with_capacity(
                17,
                SpanReader::new(Cursor::new(&wire), &spans, wire.len() as u64).unwrap(),
            );
            let mut frames = FrameProof::new(&spans, wire.len() as u64).unwrap();
            let proof = wal::probe::prove_prefix_with(
                &mut reader,
                wire.len() as u64,
                10,
                u64::MAX,
                folded,
                None,
                u64::MAX,
                None,
                Path::new("private-import-proof"),
                &budget,
                &mut |observation| frames.observe(observation),
            )
            .unwrap();
            assert_eq!(proof.head.offset, wire.len() as u64);
            frames.finish().unwrap();
            reader.into_inner().finish().unwrap();
        }
    }
}
