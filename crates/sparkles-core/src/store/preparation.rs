//! Streaming permission readers. Existing unlimited scan paths remain unchanged.
use super::*;
use crate::access::Preparation;

impl Snapshot {
    pub(crate) fn scan_prepared(
        &self,
        perm: Perm,
        prefix: &[u64],
        preparation: &Preparation,
        mut each: impl FnMut([Id; 4]) -> Result<bool>,
    ) -> Result<()> {
        preparation.check()?;
        let lo = pad(prefix, 0);
        let hi = pad(prefix, u64::MAX);
        let pi = perm.index();
        let mut inserted = Delta::key_range(&self.delta.ins[pi], lo, hi).peekable();
        let mut deleted = Delta::key_range(&self.delta.del[pi], lo, hi).peekable();
        let mut stop = false;
        let mut work = 0u64;
        self.perm(perm).for_each_prepared(
            &self.cache,
            &lo,
            &hi,
            preparation,
            |block, start, end| {
                for row in start..end {
                    preparation.check()?;
                    work += 1;
                    if work.is_multiple_of(1024) {
                        preparation.stage("scanning permission rows")?;
                    }
                    let base = block.key(row);
                    while inserted.peek().is_some_and(|key| **key <= base) {
                        preparation.check()?;
                        let key = inserted.next().expect("peeked insertion");
                        if !each(perm.to_quad(key))? {
                            stop = true;
                            return Ok(false);
                        }
                    }
                    // Deleted runs must poll even when no visible row is emitted.
                    while deleted.peek().is_some_and(|key| **key < base) {
                        preparation.check()?;
                        deleted.next();
                    }
                    if deleted.peek().is_some_and(|key| **key == base) {
                        deleted.next();
                    } else if !each(perm.to_quad(&base))? {
                        stop = true;
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )?;
        if !stop {
            for key in inserted {
                preparation.check()?;
                work += 1;
                if work.is_multiple_of(1024) {
                    preparation.stage("scanning permission rows")?;
                }
                if !each(perm.to_quad(key))? {
                    break;
                }
            }
        }
        preparation.check()
    }
}
