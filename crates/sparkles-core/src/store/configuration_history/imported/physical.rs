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
