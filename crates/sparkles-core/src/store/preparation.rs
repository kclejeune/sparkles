//! Streaming permission readers. Existing unlimited scan paths remain unchanged.
use super::*;
use crate::access::Preparation;
use std::cell::Cell;

impl Snapshot {
    pub(crate) fn scan_prepared<E: From<Error>>(
        &self,
        perm: Perm,
        prefix: &[u64],
        preparation: &Preparation,
        each: impl FnMut([Id; 4]) -> std::result::Result<bool, E>,
    ) -> std::result::Result<(), E> {
        self.scan_prepared_range_inner::<E, false>(
            perm,
            pad(prefix, 0),
            pad(prefix, u64::MAX),
            preparation,
            each,
        )
    }

    pub(crate) fn scan_prepared_range<E: From<Error>>(
        &self,
        perm: Perm,
        lo: Key,
        hi: Key,
        preparation: &Preparation,
        each: impl FnMut([Id; 4]) -> std::result::Result<bool, E>,
    ) -> std::result::Result<(), E> {
        self.scan_prepared_range_inner::<E, false>(perm, lo, hi, preparation, each)
    }

    /// Check one physical graph without scanning past its first surviving quad.
    pub(crate) fn graph_exists_prepared(&self, id: Id, preparation: &Preparation) -> Result<bool> {
        let mut exists = false;
        self.scan_prepared::<Error>(Perm::Gspo, &[id.0], preparation, |_| {
            exists = true;
            Ok(false)
        })?;
        Ok(exists)
    }

    /// Visit each physical graph with live quads once in GSPO order. Once a live
    /// quad proves existence, seek past that graph in the base and delta.
    pub(crate) fn scan_graph_ids_prepared(
        &self,
        preparation: &Preparation,
        mut each: impl FnMut(Id) -> Result<()>,
    ) -> Result<()> {
        self.scan_prepared_range_inner::<Error, true>(
            Perm::Gspo,
            pad(&[], 0),
            pad(&[], u64::MAX),
            preparation,
            |quad| {
                each(quad[crate::index::G])?;
                Ok(true)
            },
        )
    }

    fn scan_prepared_range_inner<'a, E: From<Error>, const DISTINCT_FIRST: bool>(
        &'a self,
        perm: Perm,
        lo: Key,
        hi: Key,
        preparation: &Preparation,
        mut each: impl FnMut([Id; 4]) -> std::result::Result<bool, E>,
    ) -> std::result::Result<(), E> {
        preparation.check()?;
        if lo > hi {
            return Ok(());
        }
        let pi = perm.index();
        let (inserts, deletes) = (&self.delta.ins[pi], &self.delta.del[pi]);
        let mut inserted = Delta::key_range(inserts, lo, hi).peekable();
        let mut deleted = Delta::key_range(deletes, lo, hi).peekable();
        let seen = Cell::new(None);
        let mut visit = |key: &Key,
                         inserted: &mut std::iter::Peekable<keyset::Iter<'a>>,
                         deleted: &mut std::iter::Peekable<keyset::Iter<'a>>|
         -> std::result::Result<bool, E> {
            if !each(perm.to_quad(key))? {
                return Ok(false);
            }
            if DISTINCT_FIRST {
                let first = key[0];
                seen.set(Some(first));
                let Some(next) = first.checked_add(1) else {
                    return Ok(false);
                };
                let lower = [next, 0, 0, 0];
                if inserted.peek().is_some_and(|k| k[0] == first) {
                    *inserted = Delta::key_range(inserts, lower, hi).peekable();
                }
                if deleted.peek().is_some_and(|k| k[0] <= first) {
                    *deleted = Delta::key_range(deletes, lower, hi).peekable();
                }
            }
            Ok(true)
        };
        let mut stop = false;
        let mut work = 0u64;
        self.perm(perm).for_each_prepared::<E, DISTINCT_FIRST>(
            &self.cache,
            &lo,
            &hi,
            preparation,
            |first| seen.get().is_some_and(|seen| first <= seen),
            |block, start, end| -> std::result::Result<bool, E> {
                let mut row = start;
                while row < end {
                    preparation.check()?;
                    if DISTINCT_FIRST
                        && let Some(first) = seen.get()
                        && block.cols[0][row] <= first
                    {
                        row += block.cols[0][row..end].partition_point(|&value| value <= first);
                        continue;
                    }
                    work += 1;
                    if work.is_multiple_of(1024) {
                        preparation.stage("scanning permission rows")?;
                    }
                    let base = block.key(row);
                    while inserted.peek().is_some_and(|key| **key <= base) {
                        preparation.check()?;
                        let key = inserted.next().expect("peeked insertion");
                        if !visit(key, &mut inserted, &mut deleted)? {
                            stop = true;
                            return Ok(false);
                        }
                    }
                    if DISTINCT_FIRST && seen.get() == Some(base[0]) {
                        continue;
                    }
                    // Deleted runs must poll even when no visible row is emitted.
                    while deleted.peek().is_some_and(|key| **key < base) {
                        preparation.check()?;
                        deleted.next();
                    }
                    if deleted.peek().is_some_and(|key| **key == base) {
                        deleted.next();
                    } else if !visit(&base, &mut inserted, &mut deleted)? {
                        stop = true;
                        return Ok(false);
                    }
                    row += 1;
                }
                Ok(true)
            },
        )?;
        if !stop {
            while let Some(key) = inserted.next() {
                preparation.check()?;
                work += 1;
                if work.is_multiple_of(1024) {
                    preparation.stage("scanning permission rows")?;
                }
                if !visit(key, &mut inserted, &mut deleted)? {
                    break;
                }
            }
        }
        preparation.check().map_err(E::from)
    }
}
