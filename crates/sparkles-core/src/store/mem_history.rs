//! History for in-memory stores: named snapshots and the retention window keep past
//! states as snapshots, which share their structure with the live one.

use super::*;
use crate::history::{At, HistoryGone, NamedSnapshot, Pin, Retention};

impl Store {
    /// The `410` of an in-memory store's commit that no pin or window keeps.
    pub(super) fn mem_gone(
        &self,
        seq: u64,
        head: u64,
        snapshot: Option<String>,
        metadata: Option<CommitInfo>,
    ) -> Error {
        let reconstructable = self
            .mem_history
            .as_ref()
            .map_or_else(|| vec![(head, head)], |m| m.lock().reconstructable(head));
        Error::HistoryGone(Box::new(HistoryGone {
            message: format!(
                "commit {seq} is no longer reconstructable; an in-memory dataset keeps the states its snapshots and retention window hold"
            ),
            seq,
            head,
            snapshot,
            reconstructable,
            metadata,
        }))
    }

    /// Keep the live state as a past one, if the window wants it, before commit `head`
    /// replaces it. Call with the writer lock held.
    pub(super) fn remember_past(&self, head: u64) {
        let Some(mem) = &self.mem_history else { return };
        let mut m = mem.lock();
        let r = m.retention;
        if r.keep_commits.is_none_or(|n| n == 0) && r.keep_age_ms.is_none() {
            return;
        }
        let prev = self.current.load_full();
        if prev.commit < head && m.window.back().is_none_or(|s| s.commit < prev.commit) {
            m.window.push_back(Arc::new(past(&prev)));
        }
        let now = self.now_ms();
        let cat = self.catalog.lock();
        let ts = |s: u64| match s == head {
            // the new commit is not in the catalog yet: it happens now
            true => Some(now),
            false => cat.get(s).map(|c| c.timestamp_ms),
        };
        m.trim(head, now, &ts);
    }

    pub(super) fn mem_snapshots(&self) -> Vec<NamedSnapshot> {
        let Some(mem) = &self.mem_history else {
            return Vec::new();
        };
        let m = mem.lock();
        let mut v: Vec<NamedSnapshot> = m
            .pins
            .iter()
            .map(|(name, (p, _))| self.mem_named(name, p))
            .collect();
        v.sort_by(|a, b| (a.seq, &a.name).cmp(&(b.seq, &b.name)));
        v
    }

    fn mem_named(&self, name: &str, p: &Pin) -> NamedSnapshot {
        NamedSnapshot {
            name: name.to_string(),
            seq: p.seq,
            commit: self.catalog.lock().get(p.seq),
            created_ms: p.created_ms,
            note: p.note.clone(),
            expires_ms: p.expires_ms,
            generation: Some("mem".into()),
            reconstructable: true,
            warm: p.warm,
        }
    }

    pub(super) fn mem_create_snapshot(
        &self,
        name: &str,
        at: &At,
        o: &crate::history::SnapshotOptions,
    ) -> Result<(NamedSnapshot, bool)> {
        let mem = self.mem_history.as_ref().expect("an in-memory store");
        let w = self.guarded_writer();
        let r = self.resolve_with(at, w.head)?;
        let seq = r.commit.seq;
        let live = self.snapshot();
        let mut m = mem.lock();
        if let Some((p, _)) = m.pins.get(name) {
            if p.seq == seq {
                return Ok((self.mem_named(name, p), false));
            }
            return Err(Error::Conflict(format!(
                "snapshot '{name}' already pins commit {}",
                p.seq
            )));
        }
        if m.pins.len() >= self.opts.max_snapshots {
            return Err(Error::Conflict(format!(
                "history-limit: at most {} snapshots",
                self.opts.max_snapshots
            )));
        }
        let snap = if seq == live.commit {
            Arc::new(past(&live))
        } else {
            match m.get(seq) {
                Some(s) => s,
                None => {
                    drop(m);
                    return Err(self.mem_gone(seq, w.head.seq, None, Some(r.commit)));
                }
            }
        };
        let pin = Pin {
            seq,
            created_ms: self.now_ms(),
            note: o.note.clone(),
            expires_ms: o.expires_ms,
            warm: o.warm,
        };
        m.pins.insert(name.to_string(), (pin.clone(), snap));
        Ok((self.mem_named(name, &pin), true))
    }

    pub(super) fn mem_delete_snapshot(&self, name: &str) -> bool {
        self.mem_history
            .as_ref()
            .is_some_and(|m| m.lock().pins.remove(name).is_some())
    }

    pub(super) fn mem_set_retention(&self, r: Retention) {
        let Some(mem) = &self.mem_history else { return };
        let head = self.guarded_writer().head.seq;
        let now = self.now_ms();
        let mut m = mem.lock();
        m.retention = r;
        let cat = self.catalog.lock();
        let ts = |s: u64| cat.get(s).map(|c| c.timestamp_ms);
        m.trim(head, now, &ts);
    }

    /// Drop pins past their expiry; returns their names.
    pub(super) fn mem_expire(&self, now_ms: i64) -> Vec<String> {
        let Some(mem) = &self.mem_history else {
            return Vec::new();
        };
        let mut m = mem.lock();
        let gone: Vec<String> = m
            .pins
            .iter()
            .filter(|(_, (p, _))| p.expires_ms.is_some_and(|t| t <= now_ms))
            .map(|(n, _)| n.clone())
            .collect();
        for n in &gone {
            m.pins.remove(n);
        }
        gone
    }
}

/// A live snapshot kept as a past state.
fn past(s: &Snapshot) -> Snapshot {
    Snapshot {
        historical: true,
        text: None,
        version: 0,
        ..s.clone()
    }
}
