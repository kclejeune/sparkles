//! Watches on entities (spec U12, roadmap item 619).
//!
//! A watch belongs to one identity and names an entity IRI in one graph of one branch
//! of a dataset, alone (`entity`) or with its asserted subtree (`subtree`). The
//! follower of [`follow`] reads each branch's commits in order and records, for each
//! watch, the commits whose net changes name the entity or a member of its subtree as
//! subject, predicate or object in the watched graph. The owner reads the matches in
//! the UI, which is the in-app feed; every listing and every match is authorized again
//! against the caller's live access.
//!
//! Watches live in `<dataDir>/watches.json`, outside every dataset, written with the
//! private-file pattern of the auth store. A file that a newer release wrote, or one
//! that does not parse, is refused: the routes answer `503 watches-unavailable`, the
//! follower matches nothing and the file is left as it is.

pub mod follow;
pub mod http;
#[cfg(test)]
mod tests;

use crate::auth::{Identity, Kind, Principal};
use crate::state::AppState;
use chrono::{DateTime, Utc};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The file of the watches, in the data directory.
pub const FILE: &str = "watches.json";
/// The most watches of one owner.
pub const MAX_PER_OWNER: usize = 200;
/// The most watches in all.
pub const MAX_WATCHES: usize = 20_000;
/// The most matched commits a watch keeps; older ones are dropped and `overflow` set.
pub const MAX_MATCHES: usize = 200;
/// The most hierarchy edges kept for one watched graph.
pub const MAX_GRAPH_EDGES: usize = 2_000_000;
/// The members a match lists.
pub const MATCH_MEMBERS: usize = 20;
/// How long a cursor that moved without a match may wait before it is saved.
const CURSOR_SAVE: Duration = Duration::from_secs(30);

/// What a watch covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// the entity alone
    Entity,
    /// the entity and its asserted subtree
    Subtree,
}

/// Whom a watch belongs to: the identity that owns the caller's credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub kind: Kind,
    pub name: String,
    /// the groups of an OIDC or proxy owner when it last used a watch route
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

impl Owner {
    /// The key commits record as their author: `user:bob`, `oidc:…`, `local` without
    /// auth.
    pub fn key(&self) -> String {
        match self.kind {
            Kind::Local => "local".into(),
            k => format!("{}:{}", k.as_str(), self.name),
        }
    }

    /// The owner of `p`'s watches: a minted token's owner, a configured user, an OIDC
    /// or proxy account, or `local` without auth. Anonymous callers and static tokens
    /// own none.
    pub fn of(p: &Principal) -> Option<Owner> {
        if let Some(o) = &p.info.owner
            && !p.info.static_token
        {
            return Some(Owner::of_identity(o));
        }
        match p.kind {
            Kind::Local => Some(Owner {
                kind: Kind::Local,
                name: String::new(),
                groups: Vec::new(),
            }),
            Kind::User | Kind::Oidc | Kind::Proxy => Some(Owner {
                kind: p.kind,
                name: p.name.to_string(),
                groups: Vec::new(),
            }),
            Kind::Anonymous | Kind::Token => None,
        }
    }

    fn of_identity(i: &Identity) -> Owner {
        Owner {
            kind: i.kind,
            name: i.name.clone(),
            groups: i.groups.clone(),
        }
    }

    /// Whether `other` is the same owner (groups aside).
    pub fn same(&self, other: &Owner) -> bool {
        self.kind == other.kind && self.name == other.name
    }

    pub fn identity(&self) -> Identity {
        Identity {
            kind: self.kind,
            name: self.name.clone(),
            groups: self.groups.clone(),
            display_name: None,
        }
    }
}

/// One commit a watch matched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub commit: u64,
    /// when the commit was made, or when the follower read it if the change log does
    /// not say
    pub time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// the IRIs the commit's changes named that the watch covers, at most
    /// [`MATCH_MEMBERS`] of them in IRI order, none for a summary
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// how many such IRIs the commit's changes named
    #[serde(default)]
    pub count: usize,
    /// more than the `largeCommit` bound: the match is one summary entry ("N changes
    /// under X") without members
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summary: bool,
    /// an ancestor walk stopped at the `maxAncestors` bound, so the match may miss
    /// members
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// `false` when the commit's changes were not listed (a bulk commit or a gap), so
    /// it may or may not change the entity
    pub exact: bool,
    /// made by the watch's owner
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
}

/// A watch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    pub id: String,
    pub owner: Owner,
    pub dataset: String,
    /// the branch's name when it was last seen; a rename changes it
    pub branch: String,
    /// the branch's id: a branch made again under the same name is another branch
    pub branch_id: uuid::Uuid,
    /// the graph's IRI, `None` for the default graph
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    pub entity: String,
    pub scope: Scope,
    /// for a subtree, the most levels below the entity that count (`None`: all)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    pub created: String,
    /// the head when the watch was made: only later commits match
    pub since: u64,
    /// the last commit the owner marked as read
    pub seen: u64,
    /// the matched commits, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<Match>,
    /// a match was dropped because the watch held [`MAX_MATCHES`]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overflow: bool,
    /// the graph's hierarchy passed [`MAX_GRAPH_EDGES`], so the subtree is not followed
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl Watch {
    /// The matches after `seen`.
    pub fn unread(&self) -> usize {
        self.matches.iter().filter(|m| m.commit > self.seen).count()
    }
}

/// The content of the file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// the last commit the follower finished, by branch id
    #[serde(default)]
    cursors: BTreeMap<uuid::Uuid, u64>,
    #[serde(default)]
    watches: Vec<Watch>,
}

/// The watches of the server (`AppState::watches`).
#[derive(Default)]
pub struct Watches {
    inner: Mutex<Inner>,
    /// wakes the follower when the watches change
    wake: Condvar,
    /// counts every change of the set of watches, so the follower knows to rebuild
    version: AtomicU64,
}

#[derive(Default)]
struct Inner {
    /// `None` until first read; `Err` for a file that is refused
    body: Option<Result<Body, String>>,
    /// a cursor moved since the last save
    dirty: bool,
    saved: Option<Instant>,
}

/// Why a watch operation failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// the file is refused (`503`)
    Unavailable(String),
    /// a limit (`409`)
    Limit(String),
    /// no such watch of the caller (`404`)
    Missing,
    /// the change could not be saved (`500`)
    Save(String),
}

fn path(st: &AppState) -> Option<PathBuf> {
    (!st.data_dir.as_os_str().is_empty()).then(|| st.data_dir.join(FILE))
}

fn now_text(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse(bytes: &[u8]) -> Result<Body, String> {
    sparkles::formats::settings_from_slice::<Body>(FILE, bytes).map_err(|e| e.to_string())
}

impl Inner {
    fn body(&mut self, st: &AppState) -> Result<&mut Body, Refusal> {
        let b = self.body.get_or_insert_with(|| {
            let Some(p) = path(st) else {
                return Ok(Body::default());
            };
            match std::fs::read(&p) {
                Ok(bytes) => parse(&bytes).inspect_err(|e| {
                    tracing::error!(target: "sparkles::watches", "{} is refused and left as it is: {e}", p.display());
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Body::default()),
                Err(e) => {
                    tracing::error!(target: "sparkles::watches", "cannot read {}: {e}", p.display());
                    Err(format!("cannot read {FILE}: {e}"))
                }
            }
        });
        b.as_mut().map_err(|e| Refusal::Unavailable(e.clone()))
    }

    /// Write `body` to the file. On failure the file keeps its previous content.
    fn write(st: &AppState, body: &Body) -> Result<(), String> {
        let Some(p) = path(st) else { return Ok(()) };
        let mut bytes =
            sparkles::formats::settings_to_vec(FILE, body).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        write_private(&p, &bytes).map_err(|e| e.to_string())
    }

    /// Apply `f` to a copy of the watches and save the copy; the change takes effect
    /// only once it is saved.
    fn change<T>(
        &mut self,
        st: &AppState,
        f: impl FnOnce(&mut Body) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let mut next = self.body(st)?.clone();
        let out = f(&mut next)?;
        Self::write(st, &next).map_err(|e| {
            tracing::warn!(target: "sparkles::watches", "cannot write {FILE}: {e}");
            Refusal::Save(e)
        })?;
        self.body = Some(Ok(next));
        self.dirty = false;
        self.saved = Some(Instant::now());
        Ok(out)
    }
}

#[cfg(feature = "auth")]
fn write_private(p: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    crate::auth::write_private(p, bytes)
}

#[cfg(not(feature = "auth"))]
fn write_private(p: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    crate::settings::secrets::write_atomic(p, bytes, 0o600)?;
    Ok(())
}

/// What a new watch names.
#[derive(Clone, Debug)]
pub struct NewWatch {
    pub dataset: String,
    pub branch: String,
    pub branch_id: uuid::Uuid,
    pub graph: Option<String>,
    pub entity: String,
    pub scope: Scope,
    pub depth: Option<u32>,
    /// the branch's head now
    pub head: u64,
}

impl Watches {
    fn changed(&self) {
        self.version.fetch_add(1, Ordering::AcqRel);
        self.wake.notify_all();
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Every watch, and the follower cursors; `Err` when the file is refused.
    pub(crate) fn snapshot(
        &self,
        st: &AppState,
    ) -> Result<(Vec<Watch>, BTreeMap<uuid::Uuid, u64>), Refusal> {
        let mut g = self.inner.lock();
        let b = g.body(st)?;
        Ok((b.watches.clone(), b.cursors.clone()))
    }

    /// The watches of `owner`, after recording its current groups.
    pub fn list(&self, st: &AppState, owner: &Owner) -> Result<Vec<Watch>, Refusal> {
        self.refresh_groups(st, owner)?;
        let mut g = self.inner.lock();
        let b = g.body(st)?;
        Ok(b.watches
            .iter()
            .filter(|w| w.owner.same(owner))
            .cloned()
            .collect())
    }

    /// Record `owner`'s groups on its watches when they changed, so that matching uses
    /// the groups of its latest use.
    fn refresh_groups(&self, st: &AppState, owner: &Owner) -> Result<(), Refusal> {
        if !matches!(owner.kind, Kind::Oidc | Kind::Proxy) {
            return Ok(());
        }
        let mut g = self.inner.lock();
        let stale = g
            .body(st)?
            .watches
            .iter()
            .any(|w| w.owner.same(owner) && w.owner.groups != owner.groups);
        if stale {
            g.change(st, |b| {
                for w in b.watches.iter_mut().filter(|w| w.owner.same(owner)) {
                    w.owner.groups = owner.groups.clone();
                }
                Ok(())
            })?;
            drop(g);
            self.changed();
        }
        Ok(())
    }

    /// Create a watch, or answer the same watch of the same owner (`false`: it existed).
    pub fn create(
        &self,
        st: &AppState,
        owner: &Owner,
        n: NewWatch,
        now: DateTime<Utc>,
    ) -> Result<(Watch, bool), Refusal> {
        self.refresh_groups(st, owner)?;
        let mut g = self.inner.lock();
        let out = g.change(st, |b| {
            if let Some(w) = b.watches.iter().find(|w| {
                w.owner.same(owner)
                    && w.dataset == n.dataset
                    && w.branch_id == n.branch_id
                    && w.graph == n.graph
                    && w.entity == n.entity
                    && w.scope == n.scope
                    && w.depth == n.depth
            }) {
                return Ok((w.clone(), false));
            }
            if b.watches.iter().filter(|w| w.owner.same(owner)).count() >= MAX_PER_OWNER {
                return Err(Refusal::Limit(format!(
                    "an owner has at most {MAX_PER_OWNER} watches"
                )));
            }
            if b.watches.len() >= MAX_WATCHES {
                return Err(Refusal::Limit(format!(
                    "the server holds at most {MAX_WATCHES} watches"
                )));
            }
            let w = Watch {
                id: uuid::Uuid::new_v4().to_string(),
                owner: owner.clone(),
                dataset: n.dataset,
                branch: n.branch,
                branch_id: n.branch_id,
                graph: n.graph,
                entity: n.entity,
                scope: n.scope,
                depth: n.depth,
                created: now_text(now),
                since: n.head,
                seen: n.head,
                matches: Vec::new(),
                overflow: false,
                truncated: false,
            };
            b.cursors.entry(w.branch_id).or_insert(n.head);
            b.watches.push(w.clone());
            Ok((w, true))
        })?;
        drop(g);
        if out.1 {
            self.changed();
        }
        Ok(out)
    }

    /// Remove `owner`'s watch `id`.
    pub fn delete(&self, st: &AppState, owner: &Owner, id: &str) -> Result<(), Refusal> {
        let mut g = self.inner.lock();
        g.change(st, |b| {
            let i = b
                .watches
                .iter()
                .position(|w| w.id == id && w.owner.same(owner))
                .ok_or(Refusal::Missing)?;
            let w = b.watches.remove(i);
            if !b.watches.iter().any(|x| x.branch_id == w.branch_id) {
                b.cursors.remove(&w.branch_id);
            }
            Ok(())
        })?;
        drop(g);
        self.changed();
        Ok(())
    }

    /// Raise `seen` of `owner`'s watch `id` to `commit` (a lower value is kept as it is).
    pub fn seen(
        &self,
        st: &AppState,
        owner: &Owner,
        id: &str,
        commit: u64,
    ) -> Result<Watch, Refusal> {
        let mut g = self.inner.lock();
        g.change(st, |b| {
            let w = b
                .watches
                .iter_mut()
                .find(|w| w.id == id && w.owner.same(owner))
                .ok_or(Refusal::Missing)?;
            w.seen = w.seen.max(commit);
            Ok(w.clone())
        })
    }

    /// `owner`'s watch `id`.
    pub fn get(&self, st: &AppState, owner: &Owner, id: &str) -> Result<Watch, Refusal> {
        let mut g = self.inner.lock();
        g.body(st)?
            .watches
            .iter()
            .find(|w| w.id == id && w.owner.same(owner))
            .cloned()
            .ok_or(Refusal::Missing)
    }

    /// Record what the follower found on branch `branch_id` up to `cursor`: the
    /// matches by watch id, the watches whose subtree passed a bound, and the branch's
    /// current name. Saves at once when something besides the cursor changed, and
    /// otherwise at most every [`CURSOR_SAVE`].
    pub(crate) fn record(
        &self,
        st: &AppState,
        branch_id: uuid::Uuid,
        cursor: u64,
        found: Vec<(String, Match)>,
        truncated: &BTreeMap<String, bool>,
        name: Option<&str>,
    ) {
        let mut g = self.inner.lock();
        let Ok(b) = g.body(st) else { return };
        let moved = b.cursors.get(&branch_id).is_none_or(|&c| c < cursor);
        let renamed = name.is_some_and(|n| {
            b.watches
                .iter()
                .any(|w| w.branch_id == branch_id && w.branch != n)
        });
        let flags = b
            .watches
            .iter()
            .any(|w| truncated.get(&w.id).is_some_and(|&t| t != w.truncated));
        if found.is_empty() && !renamed && !flags {
            if moved {
                b.cursors.insert(branch_id, cursor);
                g.dirty = true;
            }
            if g.dirty && g.saved.is_none_or(|s| s.elapsed() >= CURSOR_SAVE) {
                let body = g.body(st).map(|b| b.clone());
                if let Ok(body) = body {
                    if let Err(e) = Inner::write(st, &body) {
                        tracing::warn!(target: "sparkles::watches", "cannot write {FILE}: {e}");
                    } else {
                        g.dirty = false;
                    }
                    g.saved = Some(Instant::now());
                }
            }
            return;
        }
        let r = g.change(st, |b| {
            if b.cursors.contains_key(&branch_id)
                || b.watches.iter().any(|w| w.branch_id == branch_id)
            {
                let c = b.cursors.entry(branch_id).or_insert(cursor);
                *c = (*c).max(cursor);
            }
            for w in b.watches.iter_mut().filter(|w| w.branch_id == branch_id) {
                if let Some(n) = name {
                    w.branch = n.to_string();
                }
                if let Some(&t) = truncated.get(&w.id) {
                    w.truncated = t;
                }
            }
            for (id, m) in found {
                let Some(w) = b.watches.iter_mut().find(|w| w.id == id) else {
                    continue;
                };
                if m.commit <= w.since || w.matches.iter().any(|x| x.commit == m.commit) {
                    continue;
                }
                let at = w.matches.partition_point(|x| x.commit < m.commit);
                w.matches.insert(at, m);
                while w.matches.len() > MAX_MATCHES {
                    w.matches.remove(0);
                    w.overflow = true;
                }
            }
            Ok(())
        });
        if let Err(e) = r {
            tracing::warn!(target: "sparkles::watches", "cannot record matches: {e:?}");
        }
    }

    /// Remove every watch of dataset `ds` (it was deleted) or of the branches `ids`.
    pub(crate) fn remove_where(&self, st: &AppState, gone: impl Fn(&Watch) -> bool) {
        let mut g = self.inner.lock();
        let any = g.body(st).is_ok_and(|b| b.watches.iter().any(&gone));
        if !any {
            return;
        }
        let r = g.change(st, |b| {
            b.watches.retain(|w| !gone(w));
            let live: std::collections::BTreeSet<_> =
                b.watches.iter().map(|w| w.branch_id).collect();
            b.cursors.retain(|k, _| live.contains(k));
            Ok(())
        });
        drop(g);
        match r {
            Ok(()) => self.changed(),
            Err(e) => tracing::warn!(target: "sparkles::watches", "cannot remove watches: {e:?}"),
        }
    }

    /// Wait until the watches change or `d` passes.
    pub(crate) fn wait(&self, seen: u64, d: Duration) {
        let mut g = self.inner.lock();
        if self.version() != seen {
            return;
        }
        let _ = self.wake.wait_for(&mut g, d);
    }

    /// The number of watches per dataset, for server administrators.
    pub fn counts(&self, st: &AppState) -> Result<BTreeMap<String, usize>, Refusal> {
        let mut g = self.inner.lock();
        let mut out = BTreeMap::new();
        for w in &g.body(st)?.watches {
            *out.entry(w.dataset.clone()).or_default() += 1;
        }
        Ok(out)
    }
}

/// Start the follower thread.
pub fn spawn(st: &Arc<AppState>) {
    follow::spawn(Arc::downgrade(st));
}
