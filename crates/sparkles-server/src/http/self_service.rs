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
use sparkles::sparql::{LocalEndpoints, QueryOptions};
use sparkles::store::Snapshot;
use std::net::IpAddr;
use std::sync::Arc;

/// The TCP listener the server bound: its scheme, the `--host` it was given and the
/// port it listens on.
#[derive(Clone, Debug)]
pub struct Listener {
    pub scheme: &'static str,
    pub host: String,
    pub port: u16,
}

impl Listener {
    /// Whether a URL host names this listener: its literal address, or a loopback name
    /// when the listener accepts loopback connections.
    fn names(&self, host: &url::Host<&str>) -> bool {
        let bound = self.host.trim_start_matches('[').trim_end_matches(']');
        let loopback = match bound.parse::<IpAddr>() {
            Ok(ip) => ip.is_loopback() || ip.is_unspecified(),
            Err(_) => bound.eq_ignore_ascii_case("localhost"),
        };
        match host {
            url::Host::Domain(d) => {
                d.eq_ignore_ascii_case(bound) || (loopback && d.eq_ignore_ascii_case("localhost"))
            }
            url::Host::Ipv4(ip) => {
                bound.parse::<IpAddr>().is_ok_and(|b| b == IpAddr::V4(*ip))
                    || (loopback && *ip == std::net::Ipv4Addr::LOCALHOST)
            }
            url::Host::Ipv6(ip) => {
                bound.parse::<IpAddr>().is_ok_and(|b| b == IpAddr::V6(*ip))
                    || (loopback && *ip == std::net::Ipv6Addr::LOCALHOST)
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
        let url = url::Url::parse(iri).ok()?;
        if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
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
    fn resolve(&self, iri: &str) -> Option<sparkles::Result<(Arc<Snapshot>, QueryOptions)>> {
        let name = self.dataset_of(iri)?;
        // a dataset this server does not serve is not one of its endpoints
        let ds = self.st.get(&name)?;
        let p = &self.principal;
        if !p.can_at(&ds.name, Endpoint::Query, Level::Read) {
            return Some(Err(sparkles::Error::NotPermitted(format!(
                "SERVICE <{iri}>: no read access to dataset '{}'",
                ds.name
            ))));
        }
        let mut opts = super::query_options(&self.st, &ds, &super::Params(Vec::new()));
        crate::auth::restrict(&mut opts, p, &ds.name, Endpoint::Query);
        install(&self.st, &mut opts, p);
        Some(Ok((ds.store.snapshot(), opts)))
    }
}
