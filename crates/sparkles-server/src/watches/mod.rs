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

pub mod deliver;
pub mod follow;
pub mod http;
#[cfg(test)]
mod tests;

use crate::auth::{EmailClaim, Identity, Kind, Principal};
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
/// How long a provider's word on an owner that changed nothing but its time may wait
/// before it is saved.
const VOUCH_SAVE: Duration = Duration::from_secs(3600);
/// The identities whose latest vouching is kept in memory before the record starts
/// again.
const VOUCHED_MAX: usize = 10_000;

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
    /// the groups of an OIDC or proxy owner as its provider or proxy last asserted them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// when the provider or proxy last vouched for an OIDC or proxy owner with these
    /// groups (RFC 3339); mail stops once it is older than `identityMaxAgeDays`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups_at: Option<String>,
    /// the address the provider or proxy verified for an OIDC or proxy owner; a
    /// configured user's address is read from the auth configuration instead
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
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
            Kind::Local => Some(Owner::named(Kind::Local, "")),
            Kind::User | Kind::Oidc | Kind::Proxy => Some(Owner::named(p.kind, &p.name)),
            Kind::Anonymous | Kind::Token => None,
        }
    }

    fn of_identity(i: &Identity) -> Owner {
        Owner {
            groups: i.groups.clone(),
            ..Owner::named(i.kind, &i.name)
        }
    }

    /// An owner without groups, freshness or address.
    pub fn named(kind: Kind, name: &str) -> Owner {
        Owner {
            kind,
            name: name.to_string(),
            groups: Vec::new(),
            groups_at: None,
            email: None,
        }
    }

    /// Take what a provider or proxy said now when it vouched for this OIDC or proxy
    /// owner. Asserted groups replace the owner's and make them fresh. Without them
    /// the principal holds no groups, so the owner's groups are fresh only when it has
    /// none. A verified address replaces the owner's, and an unverified one removes
    /// it. `true` when the groups or the address changed.
    fn vouch(&mut self, groups: Option<&[String]>, email: &EmailClaim, now: &str) -> bool {
        let mut changed = false;
        if let Some(g) = groups
            && self.groups != g
        {
            self.groups = g.to_vec();
            changed = true;
        }
        if groups.is_some() || self.groups.is_empty() {
            self.groups_at = Some(now.to_string());
        }
        let email = match email {
            EmailClaim::Verified(a) => Some(Some(a.clone())),
            EmailClaim::Unverified => Some(None),
            EmailClaim::Unknown => None,
        };
        if let Some(e) = email
            && self.email != e
        {
            self.email = e;
            changed = true;
        }
        changed
    }

    /// Copy what was vouched for from `o`.
    fn take_vouched(&mut self, o: &Owner) {
        self.groups = o.groups.clone();
        self.groups_at = o.groups_at.clone();
        self.email = o.email.clone();
    }

    /// Whether `other` is the same owner (groups aside).
    pub fn same(&self, other: &Owner) -> bool {
        self.kind == other.kind && self.name == other.name
    }

    fn same_identity(&self, who: &Identity) -> bool {
        self.kind == who.kind && self.name == who.name
    }

    pub fn identity(&self) -> Identity {
        Identity {
            kind: self.kind,
            name: self.name.clone(),
            groups: self.groups.clone(),
            display_name: None,
        }
    }

    /// Whether the provider or proxy vouched for an OIDC or proxy owner within
    /// `days` of `now`; a configured user, whose grants the configuration holds, is
    /// always current.
    pub fn current(&self, days: u32, now: DateTime<Utc>) -> bool {
        match self.kind {
            Kind::User => true,
            Kind::Oidc | Kind::Proxy => self
                .groups_at
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .is_some_and(|t| {
                    now - t.with_timezone(&Utc) <= chrono::TimeDelta::days(days.into())
                }),
            _ => false,
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

/// Every watch, the follower cursors by branch id, and the version they belong to.
pub(crate) type Snapshot = (Vec<Watch>, BTreeMap<uuid::Uuid, u64>, u64);

/// The watches of the server (`AppState::watches`).
#[derive(Default)]
pub struct Watches {
    inner: Mutex<Inner>,
    /// what providers and proxies last vouched for each OIDC or proxy identity, by
    /// owner key, and when it was last written to the owner's watches; the lock is
    /// taken before `inner`, never after
    vouched: Mutex<std::collections::HashMap<String, (Owner, Option<Instant>)>>,
    /// the matches that wait to be mailed to their owners
    pub(crate) batches: deliver::Batches,
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

/// The file's content. Unlike other settings files, a file without its `format`
/// member is refused, since a reader cannot tell which release wrote it.
fn parse(bytes: &[u8]) -> Result<Body, String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("{FILE}: {e}"))?;
    if !v.get("format").is_some_and(serde_json::Value::is_u64) {
        return Err(format!("{FILE} has no format member"));
    }
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
    /// The watches changed: count it, under the lock of `_g`, so a reader of the
    /// watches and the version together sees both or neither.
    fn bump(&self, _g: &Inner) {
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    fn changed(&self) {
        self.wake.notify_all();
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Every watch, the follower cursors and the version they belong to; `Err` when
    /// the file is refused.
    pub(crate) fn snapshot(&self, st: &AppState) -> Result<Snapshot, Refusal> {
        let mut g = self.inner.lock();
        let version = self.version();
        let b = g.body(st)?;
        Ok((b.watches.clone(), b.cursors.clone(), version))
    }

    /// The watches of `owner`.
    pub fn list(&self, st: &AppState, owner: &Owner) -> Result<Vec<Watch>, Refusal> {
        let mut g = self.inner.lock();
        let b = g.body(st)?;
        Ok(b.watches
            .iter()
            .filter(|w| w.owner.same(owner))
            .cloned()
            .collect())
    }

    /// A provider or proxy vouched for `who` (see [`Owner::vouch`]): keep its word and,
    /// when the owner has watches, record it on them, at once when the groups or the
    /// address changed and otherwise at most every [`VOUCH_SAVE`]. Changed groups make
    /// the follower authorize the owner again.
    pub fn vouched(
        &self,
        st: &AppState,
        who: &Identity,
        groups: bool,
        email: &EmailClaim,
        now: DateTime<Utc>,
    ) {
        if !matches!(who.kind, Kind::Oidc | Kind::Proxy) {
            return;
        }
        let key = Owner::named(who.kind, &who.name).key();
        let mut m = self.vouched.lock();
        if !m.contains_key(&key) && m.len() >= VOUCHED_MAX {
            m.clear();
        }
        let (o, written) = m.entry(key).or_insert_with(|| {
            // the owner's watches hold what was vouched before the server started
            let mut g = self.inner.lock();
            let o = g
                .body(st)
                .ok()
                .and_then(|b| b.watches.iter().find(|w| w.owner.same_identity(who)))
                .map(|w| w.owner.clone())
                .unwrap_or_else(|| Owner::named(who.kind, &who.name));
            (o, None)
        });
        let changed = o.vouch(groups.then_some(&who.groups[..]), email, &now_text(now));
        if !changed && written.is_some_and(|t| t.elapsed() < VOUCH_SAVE) {
            return;
        }
        let o = o.clone();
        let mut g = self.inner.lock();
        let Ok(b) = g.body(st) else { return };
        if !b.watches.iter().any(|w| w.owner.same(&o)) {
            return;
        }
        let groups_changed = b
            .watches
            .iter()
            .any(|w| w.owner.same(&o) && w.owner.groups != o.groups);
        let r = g.change(st, |b| {
            for w in b.watches.iter_mut().filter(|w| w.owner.same(&o)) {
                w.owner.take_vouched(&o);
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                if let Some((_, t)) = m.get_mut(&o.key()) {
                    *t = Some(Instant::now());
                }
                if groups_changed {
                    self.bump(&g);
                    drop(g);
                    self.changed();
                }
            }
            Err(e) => {
                tracing::warn!(target: "sparkles::watches", "cannot record the groups of {}: {e:?}", o.key());
            }
        }
    }

    /// `owner` with what its provider or proxy last vouched for since the start.
    pub(crate) fn vouched_owner(&self, owner: &Owner) -> Owner {
        let mut o = owner.clone();
        if let Some((v, _)) = self.vouched.lock().get(&owner.key()) {
            o.take_vouched(v);
        }
        o
    }

    /// Create a watch, or answer the same watch of the same owner (`false`: it existed).
    pub fn create(
        &self,
        st: &AppState,
        owner: &Owner,
        n: NewWatch,
        now: DateTime<Utc>,
    ) -> Result<(Watch, bool), Refusal> {
        // the provider's latest word on the owner, when it spoke since the start
        let vouched = self
            .vouched
            .lock()
            .get(&owner.key())
            .map(|(o, _)| o.clone());
        let mut g = self.inner.lock();
        let out = g.change(st, |b| {
            let mut owner = owner.clone();
            if let Some(v) = &vouched {
                owner.take_vouched(v);
            } else if let Some(w) = b.watches.iter().find(|w| w.owner.same(&owner)) {
                owner.take_vouched(&w.owner);
            }
            let owner = &owner;
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
            // a follower that passed the head this request read reads from it again
            let c = b.cursors.entry(w.branch_id).or_insert(n.head);
            *c = (*c).min(n.head);
            b.watches.push(w.clone());
            Ok((w, true))
        })?;
        if out.1 {
            self.bump(&g);
        }
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
        self.bump(&g);
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
    /// matches by watch id, the watches whose hierarchy is not followed, and the
    /// branch's current dataset and branch names. `known` are the watches the run
    /// matched against: the cursor stays at or below the `since` of a watch it did not
    /// know, so that watch's commits are read once the follower takes it. Saves at once
    /// when something besides the cursor changed, and otherwise at most every
    /// [`CURSOR_SAVE`]. `false` when the matches could not be saved, and the follower
    /// then reads the commits again.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        st: &AppState,
        branch_id: uuid::Uuid,
        cursor: u64,
        found: Vec<(String, Match)>,
        truncated: &BTreeMap<String, bool>,
        names: (&str, &str),
        known: &std::collections::BTreeSet<String>,
    ) -> bool {
        let (dataset, branch) = names;
        let mut g = self.inner.lock();
        let Ok(b) = g.body(st) else { return false };
        let cursor = b
            .watches
            .iter()
            .filter(|w| w.branch_id == branch_id && !known.contains(&w.id))
            .map(|w| w.since)
            .fold(cursor, u64::min);
        let moved = b.cursors.get(&branch_id).is_none_or(|&c| c < cursor);
        let renamed = b
            .watches
            .iter()
            .any(|w| w.branch_id == branch_id && (w.branch != branch || w.dataset != dataset));
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
            return true;
        }
        let mut told: Vec<(Owner, String, String, u64)> = Vec::new();
        let r = g.change(st, |b| {
            if b.watches.iter().any(|w| w.branch_id == branch_id) {
                let c = b.cursors.entry(branch_id).or_insert(cursor);
                *c = (*c).max(cursor);
            }
            for w in b.watches.iter_mut().filter(|w| w.branch_id == branch_id) {
                w.branch = branch.to_string();
                w.dataset = dataset.to_string();
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
                if !m.own {
                    told.push((w.owner.clone(), w.dataset.clone(), w.id.clone(), m.commit));
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
        drop(g);
        match r {
            Ok(()) => {
                self.batches.add(st, told);
                true
            }
            Err(e) => {
                tracing::warn!(target: "sparkles::watches", "cannot record matches, so the commits are read again: {e:?}");
                false
            }
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
        if r.is_ok() {
            self.bump(&g);
        }
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

/// Start the follower thread, and follow what providers and proxies vouch for.
pub fn spawn(st: &Arc<AppState>) {
    install(st);
    follow::spawn(Arc::downgrade(st));
}

/// Record on the watches what providers and proxies vouch for (see
/// [`Watches::vouched`]).
pub fn install(st: &Arc<AppState>) {
    #[cfg(feature = "auth")]
    if let Some(a) = &st.auth {
        let weak = Arc::downgrade(st);
        a.on_vouched(Arc::new(move |who, groups, email| {
            if let Some(st) = weak.upgrade() {
                st.watches.vouched(&st, who, groups, email, Utc::now());
            }
        }));
    }
    #[cfg(not(feature = "auth"))]
    let _ = st;
}
