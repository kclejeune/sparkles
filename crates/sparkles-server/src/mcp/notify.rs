//! Change notifications. The tool set is not fixed: stored queries and GraphQL schemas
//! come and go as tools, datasets are created and deleted, and grants decide what a
//! caller sees. A client learns of changes in two ways.
//!
//! * `subscriptions/listen` (revision `2026-07-28`): the client names what it wants,
//!   `toolsListChanged`, `resourcesListChanged` and the resources whose updates it wants,
//!   and the stream carries `notifications/tools/list_changed`,
//!   `notifications/resources/list_changed` and `notifications/resources/updated`.
//! * A session of the `initialize` era gets the two list notifications on its stream
//!   from `notifications/initialized` on.
//!
//! Each subscription looks at what its caller sees every
//! [`McpConfig::watch_interval`](super::McpConfig::watch_interval) and notifies when it
//! changed: the names of the tools it may call with the versions of the stored queries
//! behind them, the URIs of its resources, and per subscribed resource its dataset's head
//! commit, prefixes or stored query version. At most [`MAX_SUBSCRIPTIONS`] watch at once.

use super::McpServer;
use super::context::{Kind, SCHEME};
use crate::auth::Principal;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Subscriptions and session watchers at once, over all callers.
pub const MAX_SUBSCRIPTIONS: usize = 64;

/// What a subscription asks for.
#[derive(Clone, Debug, Default)]
pub struct Watch {
    pub tools: bool,
    pub resources: bool,
    /// resources whose updates it wants
    pub uris: Vec<String>,
}

/// What changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Tools,
    Resources,
    Updated(String),
}

fn hash<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

impl McpServer {
    /// The tools `p` may call, with the versions of the stored queries behind them.
    fn tools_state(&self, p: &Principal) -> u64 {
        let fixed: Vec<&str> = self.tools_for(p).iter().map(|t| t.name).collect();
        let stored: Vec<(String, u64)> = self
            .stored_tools(p)
            .into_iter()
            .map(|t| (t.name, t.stored.version.version))
            .collect();
        hash(&(fixed, stored))
    }

    /// The resources `p` may read.
    fn resources_state(&self, p: &Principal) -> u64 {
        let names: Vec<String> = self.visible(p).iter().map(|d| d.name.clone()).collect();
        hash(&names)
    }

    /// The state of resource `uri` for `p` (`None`: it is not one `p` may read).
    fn resource_state(&self, p: &Principal, uri: &str) -> Option<u64> {
        let rest = uri.strip_prefix(SCHEME)?;
        let (name, path) = rest.split_once('/')?;
        let ds = self.dataset(p, Some(name)).ok()?;
        // a dataset deleted and created again under its name is another one
        let instance = Arc::as_ptr(&ds) as usize;
        match path {
            p if p == Kind::Schema.path() => Some(hash(&(instance, ds.store.head_commit().seq))),
            p if p == Kind::Prefixes.path() => {
                Some(hash(&(instance, super::tools::dataset_prefixes(&ds))))
            }
            p => {
                let query = p.strip_prefix(Kind::Query.path())?.strip_prefix('/')?;
                let version = ds.queries.get(query, None).map(|s| s.version.version);
                Some(hash(&(instance, version)))
            }
        }
    }

    /// A watcher of what `p` sees, for the changes `w` asks for.
    pub fn watcher(&self, p: Principal, w: &Watch) -> Watcher {
        Watcher {
            tools: w.tools.then(|| self.tools_state(&p)),
            resources: w.resources.then(|| self.resources_state(&p)),
            states: w
                .uris
                .iter()
                .map(|u| (u.clone(), self.resource_state(&p, u)))
                .collect(),
            pending: VecDeque::new(),
            server: self.clone(),
            p,
        }
    }
}

/// Looks at what one caller sees, and reports what changed since it last looked.
pub struct Watcher {
    server: McpServer,
    p: Principal,
    tools: Option<u64>,
    resources: Option<u64>,
    /// subscribed resources and their states (`None`: not readable)
    states: Vec<(String, Option<u64>)>,
    /// changes found and not yet returned
    pending: VecDeque<Change>,
}

impl Watcher {
    /// The next change. It is cancel-safe: a change found is kept until returned.
    pub async fn next(&mut self) -> Change {
        loop {
            if let Some(c) = self.pending.pop_front() {
                return c;
            }
            tokio::time::sleep(self.server.cfg().watch_interval).await;
            self.look();
        }
    }

    fn look(&mut self) {
        let (s, p) = (&self.server, &self.p);
        if let Some(last) = &mut self.tools {
            let now = s.tools_state(p);
            if now != *last {
                *last = now;
                self.pending.push_back(Change::Tools);
            }
        }
        if let Some(last) = &mut self.resources {
            let now = s.resources_state(p);
            if now != *last {
                *last = now;
                self.pending.push_back(Change::Resources);
            }
        }
        for (uri, last) in &mut self.states {
            let now = s.resource_state(p, uri);
            // a resource the caller may not read reports nothing
            if now != *last && now.is_some() {
                self.pending.push_back(Change::Updated(uri.clone()));
            }
            *last = now;
        }
    }
}
