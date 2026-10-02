//! Scheduled pins, pin expiry and the periodic history tick.

use super::*;
use crate::history::{At, NamedSnapshot, Schedule, TickReport};

/// Whether `name` is a pin schedule `s` made: its prefix and a compact UTC time.
fn scheduled_by(s: &Schedule, name: &str) -> bool {
    name.strip_prefix(s.prefix.as_str()).is_some_and(|t| {
        let b = t.as_bytes();
        b.len() == 16
            && b[8] == b'T'
            && b[15] == b'Z'
            && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
    })
}

impl Store {
    /// The pin schedules.
    pub fn schedules(&self) -> Vec<Schedule> {
        if let Some(m) = &self.mem_history {
            return m.lock().schedules.clone();
        }
        self.history
            .as_ref()
            .map(|h| h.lock().schedules.clone())
            .unwrap_or_default()
    }

    /// Replace the pin schedules (durable on return). Prefixes must be distinct.
    pub fn set_schedules(&self, schedules: Vec<Schedule>) -> Result<()> {
        for (i, s) in schedules.iter().enumerate() {
            s.validate()?;
            if schedules[..i].iter().any(|t| t.prefix == s.prefix) {
                return Err(Error::invalid(format!(
                    "two schedules have the prefix {:?}",
                    s.prefix
                )));
            }
        }
        if let Some(m) = &self.mem_history {
            m.lock().schedules = schedules;
            return Ok(());
        }
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::HistoryUnsupported(
                "pin schedules need a dataset with history".into(),
            ));
        };
        let _w = self.writer.lock();
        let mut h = hist.lock();
        crate::history::write_file(root, self.dataset_id, &h.pins, h.retention, &schedules)?;
        h.schedules = schedules;
        Ok(())
    }

    /// Periodic history upkeep (a server runs it every minute): drop pins past their
    /// expiry, make the pins schedules call for and rotate their oldest out, then collect
    /// the generations nothing needs any more, the retention window's time edge included.
    pub fn history_tick(&self) -> Result<TickReport> {
        let now = self.now_ms();
        let mut report = TickReport {
            expired: self.expire_pins(now)?,
            ..Default::default()
        };
        for s in self.schedules() {
            let mut mine: Vec<NamedSnapshot> = self
                .snapshots()
                .into_iter()
                .filter(|p| scheduled_by(&s, &p.name))
                .collect();
            mine.sort_by(|a, b| (a.created_ms, &a.name).cmp(&(b.created_ms, &b.name)));
            let head = self.head_commit().seq;
            let due = mine
                .last()
                .is_none_or(|p| now - p.created_ms >= s.every_ms as i64 && p.seq != head);
            if !due {
                continue;
            }
            let name = s.name_at(now);
            let note = Some(format!("scheduled ({})", s.prefix));
            let made = match self.create_snapshot(&name, &At::Head, note.clone()) {
                // the limits are full: make room with the oldest of this schedule
                Err(Error::Conflict(m)) if m.starts_with("history-limit") && !mine.is_empty() => {
                    let oldest = mine.remove(0);
                    self.delete_snapshot(&oldest.name)?;
                    report.rotated.push(oldest.name);
                    self.create_snapshot(&name, &At::Head, note)
                }
                r => r,
            };
            match made {
                Ok((p, true)) => {
                    report.created.push(p.name.clone());
                    mine.push(p);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("scheduled snapshot {name}: {e}"),
            }
            while mine.len() > s.keep_last as usize {
                let oldest = mine.remove(0);
                self.delete_snapshot(&oldest.name)?;
                report.rotated.push(oldest.name);
            }
        }
        if let Some(hist) = &self.history {
            let w = self.writer.lock();
            let current = commit::generation_number(&self.snapshot().generation.name);
            self.collect_locked(&mut hist.lock(), current, w.head.seq);
        } else if let Some(m) = &self.mem_history {
            let head = self.writer.lock().head.seq;
            let mut m = m.lock();
            let cat = self.catalog.lock();
            let ts = |s: u64| cat.get(s).map(|c| c.timestamp_ms);
            m.trim(head, now, &ts);
        }
        Ok(report)
    }

    /// Remove the pins whose expiry has passed (durably); returns their names.
    fn expire_pins(&self, now: i64) -> Result<Vec<String>> {
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Ok(self.mem_expire(now));
        };
        let w = self.writer.lock();
        let current = commit::generation_number(&self.snapshot().generation.name);
        let mut h = hist.lock();
        let gone: Vec<String> = h
            .pins
            .iter()
            .filter(|(_, p)| p.expires_ms.is_some_and(|t| t <= now))
            .map(|(n, _)| n.clone())
            .collect();
        if gone.is_empty() {
            return Ok(gone);
        }
        let mut pins = h.pins.clone();
        for n in &gone {
            pins.remove(n);
        }
        crate::history::write_file(root, self.dataset_id, &pins, h.retention, &h.schedules)?;
        h.pins = pins;
        self.collect_locked(&mut h, current, w.head.seq);
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_names() {
        let s = Schedule {
            prefix: "daily-".into(),
            every_ms: 86_400_000,
            keep_last: 7,
        };
        let n = s.name_at(1_727_704_991_482);
        assert_eq!(n, "daily-20240930T140311Z");
        assert!(crate::history::valid_name(&n));
        assert!(scheduled_by(&s, &n));
        assert!(!scheduled_by(&s, "daily-release"));
        assert!(!scheduled_by(&s, "weekly-20240930T140311Z"));
    }
}
