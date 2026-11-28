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
/// grant that names the view.
fn decide_restricted(
    name: &str,
    caller: &Arc<GraphAccess>,
    view: Option<&Arc<GraphAccess>>,
) -> ViewDecision {
    let contained = match view {
        Some(v) => caller.read_contains(v),
        None => caller.read_contains(&Arc::new(GraphAccess::all())),
    };
    match contained {
        Ok(()) => ViewDecision::Allow(ViewAllow::Contained),
        Err(_) if caller.views.grants(name) => ViewDecision::Allow(ViewAllow::Declassified),
        Err(gap) => ViewDecision::Deny(gap),
    }
}

/// Whether `caller` reads every quad of the dataset, so that it reads every view.
fn unrestricted(caller: Option<&Arc<GraphAccess>>) -> bool {
    caller.is_none_or(|c| c.settled().reads_everything())
}

/// The decision for `caller` on view `name`, whose principal reads `view` (`None`:
/// every quad), without a cache or an audit record.
pub fn decide(
    name: &str,
    caller: Option<&Arc<GraphAccess>>,
    view: Option<&Arc<GraphAccess>>,
) -> ViewDecision {
    match caller {
        _ if unrestricted(caller) => ViewDecision::Allow(ViewAllow::Unrestricted),
        Some(c) => decide_restricted(name, &c.guards_closed(), view),
        None => unreachable!("an absent caller is unrestricted"),
    }
}

/// The most decisions the cache keeps. The cache is emptied when it would grow past
/// this, so it holds at most this many entries, each two shared views and a name.
pub const AUTH_CACHE_ENTRIES: usize = 1024;

struct Memo {
    name: Box<str>,
    caller: Arc<GraphAccess>,
    view: Option<Arc<GraphAccess>>,
    decision: ViewDecision,
}

/// The decisions by caller and view. An entry is found by a hash of the view's
/// name and of both grant sets, and is used only when the name and both grant sets
/// are equal to the request's, so a change of the caller's grants, of the view's
/// principal or of the policy that resolves it misses the cache whatever the hash.
/// The runtime also empties the cache whenever a view is defined, attached under a
/// new policy or removed.
#[derive(Default)]
pub(super) struct AuthCache {
    map: Mutex<FxHashMap<u64, Vec<Memo>>>,
    entries: AtomicUsize,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// Counters of the decision cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewAuthStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
}

impl AuthCache {
    fn key(name: &str, caller: &GraphAccess, view: Option<&GraphAccess>) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut h);
        caller.hash(&mut h);
        view.hash(&mut h);
        h.finish()
    }

    /// The decision for a restricted `caller` whose guards are closed
    /// ([`GraphAccess::guards_closed`]), and whether it was cached.
    fn decide(
        &self,
        name: &str,
        caller: &Arc<GraphAccess>,
        view: Option<&Arc<GraphAccess>>,
    ) -> (ViewDecision, bool) {
        let key = Self::key(name, caller, view.map(|v| &**v));
        if let Some(memos) = self.map.lock().get(&key)
            && let Some(m) = memos.iter().find(|m| {
                &*m.name == name && *m.caller == **caller && m.view.as_deref() == view.map(|v| &**v)
            })
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return (m.decision, true);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let decision = decide_restricted(name, caller, view);
        let mut map = self.map.lock();
        if self.entries.load(Ordering::Relaxed) >= AUTH_CACHE_ENTRIES {
            map.clear();
            self.entries.store(0, Ordering::Relaxed);
        }
        let memos = map.entry(key).or_default();
        if !memos.iter().any(|m| {
            &*m.name == name && *m.caller == **caller && m.view.as_deref() == view.map(|v| &**v)
        }) {
            memos.push(Memo {
                name: name.into(),
                caller: caller.clone(),
                view: view.cloned(),
                decision,
            });
            self.entries.fetch_add(1, Ordering::Relaxed);
        }
        (decision, false)
    }

    pub(super) fn clear(&self) {
        let mut map = self.map.lock();
        map.clear();
        self.entries.store(0, Ordering::Relaxed);
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
    /// through the cache, and whether the cache held the decision. An unrestricted
    /// caller is neither cached nor counted.
    pub(super) fn decide_cached(
        &self,
        name: &str,
        caller: Option<&Arc<GraphAccess>>,
        view: Option<&Arc<GraphAccess>>,
    ) -> (ViewDecision, bool) {
        match caller {
            Some(c) if !unrestricted(caller) => self.auth.decide(name, &c.guards_closed(), view),
            _ => (ViewDecision::Allow(ViewAllow::Unrestricted), false),
        }
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
    pub fn authorize_view(
        &self,
        name: &str,
        caller: Option<&Arc<GraphAccess>>,
        op: ViewOp,
    ) -> Result<ViewDecision> {
        let view = {
            let reg = self.reg.lock();
            let e = reg.view(name)?;
            e.cell.as_ref().and(e.access.clone())
        };
        let decided = self.decide_cached(name, caller, view.as_ref());
        self.audit(name, caller, op, decided);
        Ok(decided.0)
    }

    /// The counters of the decision cache.
    pub fn view_auth_stats(&self) -> ViewAuthStats {
        self.auth.stats()
    }
}
