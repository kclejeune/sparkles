//! SERVICE calls to this server's own query endpoints, run in process
//! (X08-P1-nested-service).
//!
//! A query that calls `SERVICE <http://this-server/ds/sparql>` used to send an HTTP
//! request to itself. That request was admitted like any other, so it waited for pool
//! memory that its caller held and answered `503` at the admission deadline. A SERVICE
//! IRI that exactly names one of this server's query endpoints now runs in process under
//! the caller's principal, grant, deadline and cancellation. Exactly means the
//! configured public base URL, or the scheme, host and port of the bound listener with
//! its literal address or, when the listener accepts loopback connections, a loopback
//! name. Any other name, including a DNS name that resolves to this server, stays an
//! ordinary HTTP call, since a name is not proof of identity.

use crate::auth::{Endpoint, Level, Principal};
use crate::state::AppState;
use sparkles::sparql::{LocalEndpoints, LocalTarget, QueryOptions};
use std::net::IpAddr;
use std::sync::Arc;

/// The TCP listener the server bound: its scheme, and the address and port it listens
/// on as the socket reports them, so that a `--host` given as a DNS name counts by the
/// address it bound and not by the name.
#[derive(Clone, Debug)]
pub struct Listener {
    pub scheme: &'static str,
    pub ip: IpAddr,
    pub port: u16,
}

impl Listener {
    /// Whether a URL host names this listener: its literal address, or a loopback name
    /// when the listener accepts loopback connections.
    fn names(&self, host: &url::Host<&str>) -> bool {
        let loopback = self.ip.is_loopback() || self.ip.is_unspecified();
        match host {
            url::Host::Domain(d) => loopback && d.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => {
                self.ip == IpAddr::V4(*ip) || (loopback && *ip == std::net::Ipv4Addr::LOCALHOST)
            }
            url::Host::Ipv6(ip) => {
                self.ip == IpAddr::V6(*ip) || (loopback && *ip == std::net::Ipv6Addr::LOCALHOST)
            }
        }
    }
}

/// The resolver a request's queries carry: this server and the caller.
pub struct SelfEndpoints {
    st: Arc<AppState>,
    principal: Principal,
}

impl std::fmt::Debug for SelfEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfEndpoints").finish_non_exhaustive()
    }
}

/// Let the queries of `opts` answer SERVICE calls to this server in process, as `p`.
pub fn install(st: &Arc<AppState>, opts: &mut QueryOptions, p: &Principal) {
    opts.local_endpoints = Some(Arc::new(SelfEndpoints {
        st: st.clone(),
        // the IRI names a dataset, not the branch of the caller's request
        principal: p.clone().on_branch(None),
    }));
}

impl SelfEndpoints {
    /// The dataset whose query endpoint `iri` names exactly, if it is this server's.
    fn dataset_of(&self, iri: &str) -> Option<String> {
        // user information of any form, even an empty one that the parser drops, makes
        // the IRI something other than this server's endpoint
        let authority = iri.split_once("://")?.1;
        let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
        if authority.contains('@') {
            return None;
        }
        let url = url::Url::parse(iri).ok()?;
        if url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let port = url.port_or_known_default()?;
        let host = url.host()?;
        let mut base = None;
        if let Some(public) = crate::auth::public_url(&self.st)
            && let Ok(p) = url::Url::parse(&public)
            && p.scheme() == url.scheme()
            && p.host() == Some(host.clone())
            && p.port_or_known_default() == Some(port)
        {
            base = Some(p.path().trim_end_matches('/').to_string());
        }
        if base.is_none()
            && let Some(l) = self.st.listener.get()
            && l.scheme == url.scheme()
            && l.port == port
            && l.names(&host)
        {
            base = Some(String::new());
        }
        let rest = url.path().strip_prefix(base?.as_str())?;
        let mut segments = rest.strip_prefix('/')?.split('/');
        let ds = segments.next()?;
        match (segments.next(), segments.next()) {
            (None, _) | (Some("sparql" | "query"), None) => {}
            _ => return None,
        }
        let ds = percent_encoding::percent_decode_str(ds)
            .decode_utf8()
            .ok()?;
        // a branch (`ds@branch`) or a path that is not a dataset name stays remote
        if ds.is_empty() || ds.contains(['@', '/']) {
            return None;
        }
        Some(ds.into_owned())
    }
}

impl LocalEndpoints for SelfEndpoints {
    fn resolve(&self, iri: &str) -> Option<sparkles::Result<LocalTarget>> {
        let name = self.dataset_of(iri)?;
        let p = &self.principal;
        // Access is checked before existence, so a caller learns nothing about a
        // dataset it may not see: a hidden one and a missing one are refused alike.
        if !p.can_at(&name, Endpoint::Query, Level::Read) {
            return Some(Err(sparkles::Error::NotPermitted(format!(
                "SERVICE <{iri}>: no read access to dataset '{name}'"
            ))));
        }
        // the availability checks of a direct request (`restoring_guard`): a dataset
        // being restored or taken offline is out of service, and the handle the check
        // returns keeps a restore from swapping the dataset out while the call reads it
        let held = match self.st.catalog.get_for_request(&name) {
            Ok(held) => held,
            Err(task) => {
                return Some(Err(sparkles::Error::Service(format!(
                    "SERVICE <{iri}>: dataset /{name} is being restored (task {task})"
                ))));
            }
        };
        // a dataset this server does not serve is not one of its endpoints
        let ds = self.st.get(&name)?;
        if ds.offline.load(std::sync::atomic::Ordering::Relaxed) {
            return Some(Err(sparkles::Error::Service(format!(
                "SERVICE <{iri}>: dataset /{name} is offline"
            ))));
        }
        let mut opts = super::query_options(&self.st, &ds, &super::Params(Vec::new()));
        crate::auth::restrict(&mut opts, p, &ds.name, Endpoint::Query);
        install(&self.st, &mut opts, p);
        Some(Ok(LocalTarget {
            snap: ds.store.snapshot(),
            options: opts,
            hold: held.map(|h| Box::new(h) as Box<dyn std::any::Any + Send + Sync>),
        }))
    }
}
