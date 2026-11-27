//! Defining, listing and removing views (spec F13 §2.1).

use super::*;

impl Runtime {
    /// Define or replace view `name` (spec F13 §2.1) and attach it; the first
    /// evaluation runs on the next round. Returns the view with its plan.
    pub fn put(&self, store: &Store, name: &str, def: ViewDef) -> Result<ViewInfo> {
        let _operation = Operation::enter();
        if !views::valid_name(name) {
            return Err(crate::incr::error(
                "invalid-view",
                format!("{name:?} cannot name a view: use letters, digits, '_' and '-'"),
            ));
        }
        def.check()?;
        // what a backup can restore: a larger definition is refused here, not there
        let size = views::to_bytes(&def, None)?.len() as u64;
        let max = crate::formats::MAX_DEFINITION_BYTES;
        if size > max {
            return Err(crate::incr::error(
                "invalid-view",
                format!("the definition is {size} bytes, larger than {max}"),
            ));
        }
        let host = self.host()?;
        let text = Self::view_text(host.as_ref(), &def)?;
        let q = self.parse(host.as_ref(), &text)?;
        if matches!(q, Query::Ask { .. } | Query::Describe { .. }) {
            return Err(crate::incr::error(
                "invalid-view",
                "a view is a SELECT or CONSTRUCT query",
            ));
        }
        let access = host.access(def.principal.as_deref())?;
        compile(&q, whole_reason(access.as_ref()))?;
        let mut deferred = DeferredDrops::default();
        let mut need_gc = false;
        self.refresh_mode();
        let (res, target, lagging) = {
            let mut reg = self.reg.lock();
            reg.writable()?;
            let (target, lagging) = self.lagging_cells(&mut reg);
            // a new definition is the operator's: it runs, and no longer waits for a check
            if let Some(root) = &self.root {
                views::write(root, name, &def)?;
            }
            if let Some(c) = reg.views.remove(name).and_then(|old| old.cell) {
                if c.release_view(name) == 0 {
                    need_gc = true;
                }
                deferred.cells.push(c);
            }
            reg.views
                .insert(name.to_string(), ViewEntry::new(name, def, None));
            *self.defs.write() = reg.defs();
            let res = self.attach(store, &mut reg, name, &mut deferred.retired);
            (res, target, lagging)
        };
        let any_busy = if !lagging.is_empty() {
            self.try_switch_cells(&lagging, &target, &mut deferred)
        } else {
            false
        };
        drop(deferred);
        if !lagging.is_empty() && !any_busy {
            self.mark_mode_switched(target.generation);
        }
        if need_gc {
            self.gc();
        }
        res?;
        self.kick();
        self.get(store, name)
    }

    /// Remove view `name` and its state.
    pub fn delete(&self, name: &str) -> Result<()> {
        let mut detached = None;
        let mut need_gc = false;
        {
            let mut reg = self.reg.lock();
            reg.writable()?;
            let old = reg.views.remove(name).ok_or_else(|| not_found(name))?;
            if let Some(c) = old.cell {
                if c.release_view(name) == 0 {
                    need_gc = true;
                }
                detached = Some(c);
            }
            if let Some(root) = &self.root {
                views::remove(root, name)?;
            }
            *self.defs.write() = reg.defs();
        }
        drop(detached);
        if need_gc {
            self.gc();
        }
        self.kick();
        Ok(())
    }

    /// Evaluate view `name` again from scratch on the next round, lifting a
    /// suspension. A restored or cloned view resumes: it attaches on the next round,
    /// which resolves its principal and its stored query under the current policy.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        let mut reg = self.reg.lock();
        let e = reg.views.get_mut(name).ok_or_else(|| not_found(name))?;
        // a resumed view's definition is written as it runs, durably, before the
        // suspension is lifted
        if e.paused {
            if let Some(root) = &self.root {
                views::write(root, name, &e.def)?;
            }
            e.paused = false;
        }
        e.suspended = None;
        e.error = None;
        if let Some(c) = &e.cell {
            c.info.lock().status = Status::Building;
        }
        drop(reg);
        self.kick();
        Ok(())
    }

    pub(super) fn info(&self, name: &str, e: &ViewEntry, head: u64, reg: &Registry) -> ViewInfo {
        let snapshot = e.cell.as_ref().map(|cell| cell.info.lock().clone());
        self.info_from_snapshot(name, e, head, reg, snapshot)
    }

    fn info_from_snapshot(
        &self,
        name: &str,
        e: &ViewEntry,
        head: u64,
        reg: &Registry,
        snapshot: Option<cell::Info>,
    ) -> ViewInfo {
        let mut v = ViewInfo {
            name: name.to_string(),
            definition: e.def.clone(),
            state: ViewState::Building,
            commit: None,
            columns: Vec::new(),
            rows: 0,
            state_bytes: 0,
            passes: 0,
            skipped: 0,
            pass_ms: 0.0,
            recompute_ms: 0.0,
            evaluation: None,
            evaluation_reason: None,
            plan: None,
        };
        if let Some(why) = &e.suspended {
            v.state = ViewState::Suspended {
                reason: why.clone(),
            };
        } else if let Some(err) = &e.error {
            v.state = ViewState::Failed { error: err.clone() };
        }
        let Some(i) = snapshot else {
            return v;
        };
        let stale_mode = !self.mode_is_current(i.mode_gen, reg.host_seq);
        if i.status_ready() && !stale_mode {
            v.commit = Some(i.applied);
        }
        v.columns = i.columns;
        let whole = if stale_mode {
            whole_reason(e.access.as_ref())
                .map(str::to_string)
                .or_else(|| reg.inference.as_ref().map(|inf| inf.reason().to_string()))
        } else {
            i.whole
        };
        v.evaluation = Some(if whole.is_some() {
            crate::incr::ops::Mode::Recompute
        } else {
            crate::incr::ops::Mode::Incremental
        });
        v.evaluation_reason = whole;
        v.plan = (!stale_mode).then_some(i.plan);
        v.rows = if stale_mode { 0 } else { i.rows };
        v.state_bytes = i.bytes;
        v.passes = i.passes;
        v.skipped = i.skipped;
        v.pass_ms = i.pass_time.as_secs_f64() * 1e3;
        v.recompute_ms = i.recompute_time.as_secs_f64() * 1e3;
        v.state = match i.status {
            _ if stale_mode => ViewState::Building,
            Status::Building => ViewState::Building,
            Status::Ready if i.applied >= head => ViewState::Current,
            Status::Ready => ViewState::Behind {
                commits: head - i.applied,
            },
            Status::Failed { code, message } if code != "view-failed" => ViewState::Suspended {
                reason: format!("{code}: {message}"),
            },
            Status::Failed { message, .. } => ViewState::Failed { error: message },
        };
        v
    }

    /// Whether a reader with `caller` may read view `name`'s rows and what they
    /// disclose, such as their count.
    pub fn readable_by(&self, name: &str, caller: Option<&Arc<GraphAccess>>) -> bool {
        let reg = self.reg.lock();
        reg.views
            .get(name)
            .is_some_and(|e| covers(caller, e.access.as_ref()))
    }

    /// View `name` with its state.
    pub fn get(&self, store: &Store, name: &str) -> Result<ViewInfo> {
        let _operation = Operation::enter();
        let head = store.snapshot().commit;
        self.sync_mode_nonblocking();
        let reg = self.reg.lock();
        let e = reg.view(name)?;
        Ok(self.info(name, e, head, &reg))
    }

    /// Every view with its state, by name.
    pub fn list(&self, store: &Store) -> Vec<ViewInfo> {
        let _operation = Operation::enter();
        let head = store.snapshot().commit;
        self.sync_mode_nonblocking();
        let reg = self.reg.lock();
        reg.views
            .iter()
            .map(|(n, e)| self.info(n, e, head, &reg))
            .collect()
    }

    /// The names and definitions of the views.
    pub fn definitions(&self) -> Vec<(String, ViewDef)> {
        self.reg
            .lock()
            .views
            .iter()
            .map(|(n, e)| (n.clone(), e.def.clone()))
            .collect()
    }

    /// Wait until view `name` has left `building`, at most `timeout`.
    pub fn wait_built(&self, name: &str, timeout: Duration) -> Result<ViewInfo> {
        let _operation = Operation::enter();
        let until = Instant::now() + timeout;
        let mut last_info: Option<ViewInfo> = None;
        let mut final_check = false;
        loop {
            let round_start = *self.epoch.lock();
            let now = Instant::now();
            let reg_guard = if final_check || now >= until {
                self.reg.try_lock()
            } else {
                self.reg.try_lock_until((now + read::WAIT_STEP).min(until))
            };
            let observed = if let Some(reg) = reg_guard {
                let e = reg.view(name)?;
                let snapshot = e
                    .cell
                    .as_ref()
                    .and_then(|c| c.info.try_lock().map(|i| i.clone()));
                if snapshot.is_some() || e.cell.is_none() {
                    let head = snapshot.as_ref().map_or(0, |i| i.applied);
                    let info = self.info_from_snapshot(name, e, head, &reg, snapshot);
                    #[cfg(test)]
                    if let Some(hook) = self.wait_info_hook.lock().clone() {
                        hook();
                    }
                    Some(info)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(info) = observed {
                if info.state != ViewState::Building {
                    return Ok(info);
                }
                last_info = Some(info);
            }
            if final_check || Instant::now() >= until {
                return last_info.ok_or(Error::Timeout);
            }
            self.kick();
            let step = (Instant::now() + read::WAIT_STEP).min(until);
            #[cfg(test)]
            self.before_observer_wait(until);
            self.wait_round_since(round_start, step);
            final_check = Instant::now() >= until;
        }
    }
}
