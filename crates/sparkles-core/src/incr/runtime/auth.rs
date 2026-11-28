//! Who may read a view (spec F13 §6.1): the decision, its cache per caller and view,
//! and the audit records of the decisions.
//!
//! A caller reads a view when it reads every quad of the dataset, when its grants
//! contain the grants of the view's principal ([`GraphAccess::read_contains`]), or when
//! a grant names the view (`view:{name}`, [`crate::access::ViewGrants`]), which
//! declassifies it. Everything else is refused, including a containment that the
//! comparison of grant sets cannot prove.

use super::*;
use crate::access::Gap;
use std::hash::{Hash, Hasher};

/// What a caller asks to do with a view, as audit records name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewOp {
    /// read its rows, as `SERVICE view:read` and the library's reads do
    Read,
    /// follow its rows as they change
    Watch,
}

impl ViewOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewOp::Read => "read",
            ViewOp::Watch => "watch",
        }
    }
}

/// Why a caller may read a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewAllow {
    /// the caller reads every quad of the dataset
    Unrestricted,
    /// the caller's grants contain those of the view's principal
    Contained,
    /// a grant names the view
    Declassified,
}

/// Whether a caller may read a view, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewDecision {
    Allow(ViewAllow),
    /// refused, with the category of the first gap the comparison found
    Deny(Gap),
}

impl ViewDecision {
    pub fn allowed(self) -> bool {
        matches!(self, ViewDecision::Allow(_))
    }

    /// `allow` or `deny`.
    pub fn as_str(self) -> &'static str {
        match self {
            ViewDecision::Allow(_) => "allow",
            ViewDecision::Deny(_) => "deny",
        }
    }

    /// Why: `unrestricted`, `contained` or `declassified` for an allowed read, and the
    /// gap's category for a refused one.
    pub fn reason(self) -> &'static str {
        match self {
            ViewDecision::Allow(ViewAllow::Unrestricted) => "unrestricted",
            ViewDecision::Allow(ViewAllow::Contained) => "contained",
            ViewDecision::Allow(ViewAllow::Declassified) => "declassified",
            ViewDecision::Deny(gap) => gap.as_str(),
        }
    }
}

/// One decision as the audit log records it. It names the view, the operation, the
/// decision and its category, and the caller's audit identity, and never a graph, a
/// protection, a row or a credential.
#[derive(Clone, Copy, Debug)]
pub struct ViewAccessEvent<'a> {
    pub view: &'a str,
    pub op: ViewOp,
    pub decision: ViewDecision,
    /// the decision came from the cache
    pub cached: bool,
    /// the caller as its grants name it for audit records (`None`: not named)
    pub caller: Option<&'a str>,
}

impl ViewAccessEvent<'_> {
    /// Write the event to the `sparkles::audit` log target, for dataset `dataset`
    /// when the host names it.
    pub fn log(&self, dataset: Option<&str>) {
        tracing::info!(
            target: "sparkles::audit",
            event = "view_access",
            dataset = dataset.unwrap_or("-"),
            view = self.view,
            op = self.op.as_str(),
            decision = self.decision.as_str(),
            reason = self.decision.reason(),
            cached = self.cached,
            principal = self.caller.unwrap_or("-"),
        );
    }
}

/// The decision for a caller who is not unrestricted: containment first, then a
/// grant that names the view. `caller`'s guards are closed, and `binds` tells whether
/// the caller they were closed from decides a guard at each snapshot
/// ([`GraphAccess::binds_guards`]). Without a name (`None`), only containment counts.
fn decide_restricted(
    name: Option<&str>,
    caller: &Arc<GraphAccess>,
    binds: bool,
    view: Option<&Arc<GraphAccess>>,
) -> ViewDecision {
    let contained = match view {
        Some(v) => caller.read_contains_binding(v, binds),
        None => caller.read_contains_binding(&Arc::new(GraphAccess::all()), binds),
    };
    match contained {
        Ok(()) => ViewDecision::Allow(ViewAllow::Contained),
        Err(_) if name.is_some_and(|n| caller.views.grants(n)) => {
            ViewDecision::Allow(ViewAllow::Declassified)
        }
        Err(gap) => ViewDecision::Deny(gap),
    }
}

/// Whether `caller` reads every quad of the dataset, so that it reads every view.
fn unrestricted(caller: Option<&Arc<GraphAccess>>) -> bool {
    caller.is_none_or(|c| c.settled().reads_everything())
}

/// The decision for `caller` on view `name`, whose principal reads `view` (`None`:
/// every quad), without a cache or an audit record. `caller: None` is a reader of
/// every quad, so a surface that decides for a request must first check that the
/// request may read the dataset at all.
pub fn decide(
    name: &str,
    caller: Option<&Arc<GraphAccess>>,
    view: Option<&Arc<GraphAccess>>,
) -> ViewDecision {
    decide_named(Some(name), caller, view)
}

/// [`decide`] for a view that a grant may name (`Some`), or for containment alone
/// (`None`).
pub(super) fn decide_named(
    name: Option<&str>,
    caller: Option<&Arc<GraphAccess>>,
    view: Option<&Arc<GraphAccess>>,
) -> ViewDecision {
    match caller {
        _ if unrestricted(caller) => ViewDecision::Allow(ViewAllow::Unrestricted),
        Some(c) => decide_restricted(name, &c.guards_closed(), c.binds_guards(), view),
        None => unreachable!("an absent caller is unrestricted"),
    }
}

/// The most decisions the cache keeps. The cache is emptied when it would grow past
/// this, so it holds at most this many entries, each two shared views and a name.
pub const AUTH_CACHE_ENTRIES: usize = 1024;

struct Memo {
    name: Box<str>,
    caller: Arc<GraphAccess>,
    binds: bool,
    view: Option<Arc<GraphAccess>>,
    decision: ViewDecision,
}

impl Memo {
    fn is(
        &self,
        name: &str,
        caller: &GraphAccess,
        binds: bool,
        view: Option<&GraphAccess>,
    ) -> bool {
        &*self.name == name
            && *self.caller == *caller
            && self.binds == binds
            && self.view.as_deref() == view
    }
}

/// The decisions by caller and view. An entry is found by a hash of the view's
/// name and of both grant sets, and is used only when the name and both grant sets
/// are equal to the request's, so a change of the caller's grants, of the view's
/// principal or of the policy that resolves it misses the cache whatever the hash.
/// The runtime also empties the cache whenever a view is defined, attached under a
/// new policy or removed, and each emptying starts a new generation: a decision is
/// kept or reused only for an entry read from the registry in the current generation,
/// so a decision that was made for an entry read before the cache was emptied is
/// never kept after it.
#[derive(Default)]
pub(super) struct AuthCache {
    map: Mutex<FxHashMap<u64, Vec<Memo>>>,
    entries: AtomicUsize,
    /// incremented, with `map` locked, each time the cache is emptied for a change
    /// of the views
    generation: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    /// runs on a miss before the decision is made and kept
    #[cfg(test)]
    pub(super) before_insert: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// Counters of the decision cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewAuthStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
}

impl AuthCache {
    fn key(name: &str, caller: &GraphAccess, binds: bool, view: Option<&GraphAccess>) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut h);
        caller.hash(&mut h);
        binds.hash(&mut h);
        view.hash(&mut h);
        h.finish()
    }

    /// The generation that entries read from the registry now belong to. Read with
    /// the registry locked, since every emptying happens with it locked.
    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The decision for a restricted `caller` whose guards are closed
    /// ([`GraphAccess::guards_closed`]), on an entry read in `generation`, and
    /// whether it was cached.
    fn decide(
        &self,
        name: &str,
        caller: &Arc<GraphAccess>,
        binds: bool,
        view: Option<&Arc<GraphAccess>>,
        generation: u64,
    ) -> (ViewDecision, bool) {
        let key = Self::key(name, caller, binds, view.map(|v| &**v));
        {
            let map = self.map.lock();
            if self.generation() == generation
                && let Some(m) = map.get(&key).and_then(|ms| {
                    ms.iter()
                        .find(|m| m.is(name, caller, binds, view.map(|v| &**v)))
                })
            {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return (m.decision, true);
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if let Some(hook) = self.before_insert.lock().clone() {
            hook();
        }
        let decision = decide_restricted(Some(name), caller, binds, view);
        let mut map = self.map.lock();
        // the cache was emptied after the entry was read: keep nothing for it
        if self.generation() != generation {
            return (decision, false);
        }
        if self.entries.load(Ordering::Relaxed) >= AUTH_CACHE_ENTRIES {
            map.clear();
            self.entries.store(0, Ordering::Relaxed);
        }
        let memos = map.entry(key).or_default();
        if !memos
            .iter()
            .any(|m| m.is(name, caller, binds, view.map(|v| &**v)))
        {
            memos.push(Memo {
                name: name.into(),
                caller: caller.clone(),
                binds,
                view: view.cloned(),
                decision,
            });
            self.entries.fetch_add(1, Ordering::Relaxed);
        }
        (decision, false)
    }

    /// Empty the cache for a change of the views, and start a new generation. Called
    /// with the registry locked.
    pub(super) fn clear(&self) {
        let mut map = self.map.lock();
        map.clear();
        self.entries.store(0, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn stats(&self) -> ViewAuthStats {
        ViewAuthStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries: self.entries.load(Ordering::Relaxed),
        }
    }
}

/// The refusal of a read of view `name`. It names no graph and no protection.
pub(super) fn refused(name: &str) -> Error {
    crate::incr::error(
        "view-access",
        format!("the view {name:?} reads data your grants do not cover"),
    )
}

impl Runtime {
    /// Decide whether `caller` may read view `name`, whose principal reads `view`,
    /// through the cache, and whether the cache held the decision. `generation` is the
    /// cache's [generation](AuthCache::generation) when the registry entry was read.
    /// An unrestricted caller is neither cached nor counted.
    pub(super) fn decide_cached(
        &self,
        name: &str,
        caller: Option<&Arc<GraphAccess>>,
        view: Option<&Arc<GraphAccess>>,
        generation: u64,
    ) -> (ViewDecision, bool) {
        match caller {
            Some(c) if !unrestricted(caller) => {
                self.auth
                    .decide(name, &c.guards_closed(), c.binds_guards(), view, generation)
            }
            _ => (ViewDecision::Allow(ViewAllow::Unrestricted), false),
        }
    }

    /// The grants of view `name`'s principal as decisions compare them, and the cache
    /// generation they were read in, from the registry entry `e` read with the registry
    /// locked. A view that is not attached is compared as one that reads every quad,
    /// since its principal is not resolved under the current policy.
    pub(super) fn view_grants(&self, e: &ViewEntry) -> (Option<Arc<GraphAccess>>, u64) {
        (
            e.cell.as_ref().and(e.access.clone()),
            self.auth.generation(),
        )
    }

    /// Write `decision` to the audit log when it is a refusal, a declassified read, or
    /// a decision the cache did not hold. An unrestricted read is not recorded. Called
    /// without the registry or a circuit locked.
    pub(super) fn audit(
        &self,
        name: &str,
        caller: Option<&Arc<GraphAccess>>,
        op: ViewOp,
        (decision, cached): (ViewDecision, bool),
    ) {
        let declassified = decision == ViewDecision::Allow(ViewAllow::Declassified);
        let unrestricted = decision == ViewDecision::Allow(ViewAllow::Unrestricted);
        if unrestricted || decision.allowed() && !declassified && cached {
            return;
        }
        let event = ViewAccessEvent {
            view: name,
            op,
            decision,
            cached,
            caller: caller.and_then(|c| c.views.audit_id()),
        };
        match self.current_host().1 {
            Some(host) => host.view_access(&event),
            None => event.log(None),
        }
    }

    /// Whether `caller` may `op` view `name` (spec F13 §6.1), with an audit record as
    /// [`ViewAccessEvent`] describes. A view that is not attached yet is decided as
    /// one that reads every quad, which only an unrestricted caller or a grant that
    /// names the view reads. Surfaces that read or watch a view on a caller's behalf,
    /// such as MCP tools and the web UI, ask this before they read.
    ///
    /// `caller: None` is a reader of every quad and is always allowed. A surface that
    /// decides for a request therefore checks first that the request may read the
    /// dataset at all, and passes the request's view of it only then.
    pub fn authorize_view(
        &self,
        name: &str,
        caller: Option<&Arc<GraphAccess>>,
        op: ViewOp,
    ) -> Result<ViewDecision> {
        let (view, generation) = {
            let reg = self.reg.lock();
            self.view_grants(reg.view(name)?)
        };
        let decided = self.decide_cached(name, caller, view.as_ref(), generation);
        self.audit(name, caller, op, decided);
        Ok(decided.0)
    }

    /// The counters of the decision cache.
    pub fn view_auth_stats(&self) -> ViewAuthStats {
        self.auth.stats()
    }

    /// Run `hook` on each miss of the decision cache, before the decision is made.
    #[cfg(test)]
    pub(crate) fn set_auth_miss_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.auth.before_insert.lock() = hook;
    }

    /// Run `hook` in `get_readable` and `list_readable` after the entries are read.
    #[cfg(test)]
    pub(crate) fn set_after_info_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.after_info_hook.lock() = hook;
    }

    /// Run `hook` in `open_view` before the observer wait, after the view is behind.
    #[cfg(test)]
    pub(crate) fn set_observer_wait_hook(&self, hook: Option<Arc<dyn Fn(Instant) + Send + Sync>>) {
        *self.observer_wait_hook.lock() = hook;
    }

    /// Mark a completed round so that `wait_round_after` returns.
    #[cfg(test)]
    pub(crate) fn mark_round_complete(&self) {
        self.complete_round();
    }
}
