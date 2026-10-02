//! Rate and concurrency limits per request class and client: the one limiter of the
//! server, which the auth layer's own throttles use too.
//!
//! Requests fall into four classes by their matched route and method ([`Class`]):
//! `auth` (everything under `/$/auth/`), `query`, `update` and `admin`; health checks,
//! `/$/metrics`, the UI and cheap admin reads belong to none and are never limited. Each
//! class (optionally overridden per dataset) has a [`Limit`]: a request rate with a burst,
//! enforced per client with GCRA (the "virtual scheduling" form of a token bucket, one
//! `u64` per client), and caps on the requests in flight, server-wide and per client.
//!
//! Enforcement has two stages. [`admit`] runs before authentication and applies the
//! `preauth` limit: a budget of failed credential checks per client address (and IPv6
//! /48), so that password guessing and the hashing it costs are bounded (the auth layer
//! reserves the cost of a failure through [`Admission`] before it verifies a password,
//! and an address that spent its budget has its password checks and unknown tokens
//! refused, nothing else). [`limit`] runs after authentication and applies the class
//! limits, keyed by [`ClientKeyer`] (the signed-in owner with auth).
//!
//! * Over the rate: `429 Too Many Requests` with `Retry-After`.
//! * Over a concurrency cap: `503 Service Unavailable` with `Retry-After: 1`, at once
//!   (requests are never queued, so a saturated server sheds load instead of piling up
//!   waiting requests and their memory).
//!
//! Responses of rate-limited classes carry `RateLimit-Policy` and `RateLimit` in the
//! structured-field syntax of draft-ietf-httpapi-ratelimit-headers-11.
//!
//! Limits that code charges by name rather than by route ([`Config::named`]: token mints
//! per owner, failed device-code lookups, …) share the buckets, responses and metrics.
//!
//! Clients are keyed by [`ClientKeyer`]: the peer address by default (IPv6 by its /64),
//! or the client a trusted proxy (a network, or the Unix socket) reports in the one
//! forwarding header the configuration names ([`ForwardHeader`]). The client
//! state lives in a bounded cache ([`quick_cache`], sharded, frequency-aware eviction):
//! a flood of new keys evicts other rarely seen keys, never clients with requests in
//! flight, and memory stays at about `max_keys` × 100 bytes. An evicted client that
//! still owed time is remembered in a smaller penalty cache, so churning the cache does
//! not forgive its debt. Idle buckets (fully refilled, nothing in flight) carry no
//! information and are dropped by [`RateLimiter::sweep`]. A reload keeps the state of
//! policies whose name did not change (buckets and requests in flight).

use crate::obs::{Outcome, RequestReport};
use arc_swap::ArcSwap;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------- classes ------

/// The kind of request a limit applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// every route under `/$/auth/`
    Auth,
    /// SPARQL queries, Graph Store reads, explain, SHACL validation, schema and stats
    Query,
    /// SPARQL updates, Graph Store writes, uploads
    Update,
    /// `/$/…` mutations (dataset management, compaction, backups, reasoning, …)
    Admin,
    /// every request, before authentication: authentication failures per address
    PreAuth,
}

impl Class {
    pub const COUNT: usize = 5;
    pub const ALL: [Class; Class::COUNT] = [
        Class::Auth,
        Class::Query,
        Class::Update,
        Class::Admin,
        Class::PreAuth,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Class::Auth => "auth",
            Class::Query => "query",
            Class::Update => "update",
            Class::Admin => "admin",
            Class::PreAuth => "preauth",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    fn parse(s: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// The class of a request (`None`: never limited), from its matched route template.
pub fn classify(
    route: Option<&str>,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Option<Class> {
    let path = uri.path();
    // unmatched auth paths too, so probing for endpoints is limited like using them
    if path == "/$/auth" || path.starts_with("/$/auth/") {
        return Some(Class::Auth);
    }
    let read = matches!(*method, Method::GET | Method::HEAD);
    let r = route?;
    if let Some(admin) = r.strip_prefix("/$/") {
        if admin == "ping" || admin == "metrics" || admin.starts_with("ready") {
            return None;
        }
        // reads that run queries over a dataset, and formatting (cheap, read-like work)
        if admin == "format"
            || admin.starts_with("validate/")
            || admin.starts_with("schema/")
            || admin.starts_with("stats/")
            || admin == "reason/{ds}/diagnostics"
        {
            return Some(Class::Query);
        }
        return (!read && *method != Method::OPTIONS).then_some(Class::Admin);
    }
    match r {
        "/{ds}/sparql" | "/{ds}/query" | "/{ds}/get" | "/{ds}/explain" | "/{ds}/shacl"
        | "/{ds}/shex" => Some(Class::Query),
        "/{ds}/update" | "/{ds}/upload" => Some(Class::Update),
        "/{ds}/data" | "/{ds}/{*graph}" => Some(if read { Class::Query } else { Class::Update }),
        "/{ds}" => {
            let q = uri.query().unwrap_or("");
            let has = |k: &str| form_urlencoded::parse(q.as_bytes()).any(|(a, _)| a == k);
            let ct = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if has("update") || ct.starts_with("application/sparql-update") {
                Some(Class::Update)
            } else if read || has("query") || ct.starts_with("application/sparql-query") {
                Some(Class::Query)
            } else {
                // a form body may hold a query or an update: the stricter class
                Some(Class::Update)
            }
        }
        _ => None,
    }
}

/// The `{ds}` segment of the path, for routes that have one.
fn dataset_of(route: Option<&str>, uri: &Uri) -> Option<String> {
    let pos = route?.split('/').position(|s| s == "{ds}")?;
    let seg = uri.path().split('/').nth(pos)?;
    Some(
        percent_encoding::percent_decode_str(seg)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

// ------------------------------------------------------------- configuration ------

/// `N/s`, `N/min`, …: `count` requests per `period` on average.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub count: u32,
    pub period: Duration,
}

impl Rate {
    pub fn parse(s: &str) -> Result<Rate, String> {
        let (n, unit) = s
            .split_once('/')
            .ok_or_else(|| format!("rate '{s}': expected N/s, N/min, N/h or N/d"))?;
        let count: u32 = n
            .trim()
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("rate '{s}': the count must be a positive integer"))?;
        let secs = match unit.trim() {
            "s" | "sec" | "second" => 1,
            "m" | "min" | "minute" => 60,
            "h" | "hour" => 3600,
            "d" | "day" => 86400,
            u => return Err(format!("rate '{s}': unknown unit '{u}' (s, min, h or d)")),
        };
        Ok(Rate {
            count,
            period: Duration::from_secs(secs),
        })
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Rate, D::Error> {
        Rate::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// The limits of one class (or one class on one dataset). Everything unset: unlimited.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limit {
    /// sustained request rate per client
    pub rate: Option<Rate>,
    /// requests a client may send at once after being idle (default: the rate's count)
    pub burst: Option<u32>,
    /// requests in flight, server-wide
    pub concurrency: Option<u32>,
    /// requests in flight per client
    pub client_concurrency: Option<u32>,
    /// what a response with status 401 or 403 costs, in requests (default 1): repeated
    /// failed logins exhaust the budget faster
    pub failure_cost: Option<u32>,
}

/// The `preauth` limit with authentication on and none configured: 30 failures a minute
/// per address, 60 at once.
pub const DEFAULT_PREAUTH: &str = "30/min,burst=60";

impl Limit {
    pub(crate) fn is_unlimited(&self) -> bool {
        self.rate.is_none() && self.concurrency.is_none() && self.client_concurrency.is_none()
    }

    /// `preauth` and named limits count requests or failures, not requests in flight.
    fn check_rate_only(&self, what: &str) -> Result<(), String> {
        if self.concurrency.is_some() || self.client_concurrency.is_some() {
            return Err(format!(
                "{what}: only a rate, burst and failure-cost apply (no concurrency)"
            ));
        }
        Ok(())
    }

    /// `RATE[,burst=N][,concurrency=N][,client-concurrency=N][,failure-cost=N]` or `off`.
    pub fn parse(s: &str) -> Result<Limit, String> {
        let mut l = Limit::default();
        if s.trim() == "off" {
            return Ok(l);
        }
        for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let num = |v: &str| -> Result<u32, String> {
                v.trim()
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("'{item}': expected a positive integer"))
            };
            match item.split_once('=') {
                Some(("burst", v)) => l.burst = Some(num(v)?),
                Some(("concurrency", v)) => l.concurrency = Some(num(v)?),
                Some(("client-concurrency", v)) => l.client_concurrency = Some(num(v)?),
                Some(("failure-cost", v)) => l.failure_cost = Some(num(v)?),
                Some(("rate", v)) => l.rate = Some(Rate::parse(v)?),
                Some((k, _)) => return Err(format!("unknown rate-limit setting '{k}'")),
                None => l.rate = Some(Rate::parse(item)?),
            }
        }
        if l.burst.is_some() && l.rate.is_none() {
            return Err(format!("'{s}': burst needs a rate"));
        }
        Ok(l)
    }
}

/// Rate-limit configuration: the file of `--rate-limit-config` (JSON), then the
/// `--rate-limit` flags on top.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// per class: `auth`, `query`, `update`, `admin`, `preauth`
    #[serde(default)]
    pub classes: BTreeMap<String, Limit>,
    /// per dataset and class; replaces the class limit on that dataset
    #[serde(default)]
    pub datasets: BTreeMap<String, BTreeMap<String, Limit>>,
    /// proxies whose forwarding header names the client (CIDR, address, or `unix` for
    /// the Unix socket)
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// the header they name it in: `x-forwarded-for` (default) or `forwarded`
    pub trusted_proxy_header: Option<String>,
    /// clients tracked at most (default 100000)
    pub max_keys: Option<usize>,
    /// limits charged by code under a name ([`RateLimiter::acquire`]), not by route
    /// (with what they count, for their error message: `tokens minted`)
    #[serde(skip)]
    pub named: BTreeMap<&'static str, (Limit, &'static str)>,
}

const CLASSES: &str = "auth, query, update, admin or preauth";

impl Config {
    /// Apply one `--rate-limit CLASS[@DATASET]=LIMIT` flag.
    pub fn apply_flag(&mut self, spec: &str) -> Result<(), String> {
        let (target, limit) = spec
            .split_once('=')
            .ok_or_else(|| format!("--rate-limit '{spec}': expected CLASS[@DATASET]=LIMIT"))?;
        let limit = Limit::parse(limit).map_err(|e| format!("--rate-limit '{spec}': {e}"))?;
        let (class, ds) = match target.split_once('@') {
            Some((c, d)) => (c.trim(), Some(d.trim())),
            None => (target.trim(), None),
        };
        match Class::parse(class) {
            None => {
                return Err(format!(
                    "--rate-limit '{spec}': unknown class '{class}' ({CLASSES})"
                ));
            }
            Some(Class::PreAuth) if ds.is_some() => {
                return Err(format!(
                    "--rate-limit '{spec}': preauth applies to every request, not per dataset"
                ));
            }
            Some(Class::PreAuth) => limit
                .check_rate_only("preauth")
                .map_err(|e| format!("--rate-limit '{spec}': {e}"))?,
            Some(_) => {}
        }
        match ds {
            Some(d) => {
                self.datasets
                    .entry(d.to_string())
                    .or_default()
                    .insert(class.to_string(), limit);
            }
            None => {
                self.classes.insert(class.to_string(), limit);
            }
        }
        Ok(())
    }

    /// Read a JSON configuration file.
    pub fn read(path: &std::path::Path) -> anyhow::Result<Config> {
        use anyhow::Context as _;
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let c: Config = serde_json::from_slice(&bytes)
            .with_context(|| format!("{}: invalid rate-limit configuration", path.display()))?;
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let check = |m: &BTreeMap<String, Limit>, per_dataset: bool| -> anyhow::Result<()> {
            for (c, l) in m {
                match Class::parse(c) {
                    None => anyhow::bail!("unknown rate-limit class '{c}' ({CLASSES})"),
                    Some(Class::PreAuth) if per_dataset => {
                        anyhow::bail!("preauth applies to every request, not per dataset")
                    }
                    Some(Class::PreAuth) => l.check_rate_only(c).map_err(anyhow::Error::msg)?,
                    Some(_) => {}
                }
                if l.burst.is_some() && l.rate.is_none() {
                    anyhow::bail!("{c}: burst needs a rate");
                }
            }
            Ok(())
        };
        check(&self.classes, false)?;
        for m in self.datasets.values() {
            check(m, true)?;
        }
        self.trusted().map_err(anyhow::Error::msg)?;
        Ok(())
    }

    /// The trusted proxies and the header they name the client in.
    pub fn trusted(&self) -> Result<TrustedProxies, String> {
        let header = match &self.trusted_proxy_header {
            Some(h) => ForwardHeader::parse(h)?,
            None => ForwardHeader::default(),
        };
        Ok(TrustedProxies::parse(&self.trusted_proxies)?.with_header(header))
    }

    /// Whether any limit is configured.
    pub fn is_empty(&self) -> bool {
        self.classes.values().all(Limit::is_unlimited)
            && self
                .datasets
                .values()
                .all(|m| m.values().all(Limit::is_unlimited))
            && self.named.values().all(|(l, _)| l.is_unlimited())
    }
}

// -------------------------------------------------------------- client keys ------

/// The forwarding header a trusted proxy names the client in. Only that one is read: a
/// proxy overwrites or appends to the header it sets and passes any other through, so a
/// second header would be the client's to choose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ForwardHeader {
    /// `X-Forwarded-For` (nginx, HAProxy, Caddy, Traefik, cloud load balancers)
    #[default]
    XForwardedFor,
    /// `Forwarded` (RFC 7239), the `for=` of each element
    Forwarded,
}

impl ForwardHeader {
    pub fn parse(s: &str) -> Result<ForwardHeader, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "x-forwarded-for" => Ok(ForwardHeader::XForwardedFor),
            "forwarded" => Ok(ForwardHeader::Forwarded),
            _ => Err(format!(
                "trusted proxy header '{s}': expected x-forwarded-for or forwarded"
            )),
        }
    }
}

/// Peers whose forwarding header is believed: networks, and the Unix socket (`unix`).
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies {
    nets: Vec<(IpAddr, u8)>,
    unix: bool,
    header: ForwardHeader,
}

/// Where a connection came from, as far as client keys are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAddr {
    Ip(IpAddr),
    /// the Unix socket (`--unix-socket`)
    Unix,
}

impl TrustedProxies {
    /// Parse `10.0.0.0/8`, `::1`, `fd00::/8`, `unix`, … .
    pub fn parse(items: &[String]) -> Result<TrustedProxies, String> {
        let mut t = TrustedProxies::default();
        for s in items {
            if s.trim() == "unix" {
                t.unix = true;
                continue;
            }
            let (addr, len) = match s.split_once('/') {
                Some((a, l)) => (a, Some(l)),
                None => (s.as_str(), None),
            };
            let ip: IpAddr = addr
                .trim()
                .parse()
                .map_err(|_| format!("trusted proxy '{s}': not an IP address, a CIDR or unix"))?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let len = match len {
                Some(l) => l
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|l| *l <= max)
                    .ok_or_else(|| format!("trusted proxy '{s}': bad prefix length"))?,
                None => max,
            };
            t.nets.push((ip, len));
        }
        Ok(t)
    }

    /// The same proxies, naming the client in `header`.
    pub fn with_header(mut self, header: ForwardHeader) -> TrustedProxies {
        self.header = header;
        self
    }

    /// Whether the Unix socket is trusted.
    pub fn unix(&self) -> bool {
        self.unix
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.nets.iter().any(|&(net, len)| match (net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        })
    }

    /// Whether `peer`'s forwarding header is believed.
    pub fn trusts(&self, peer: PeerAddr) -> bool {
        match peer {
            PeerAddr::Ip(ip) => self.contains(ip),
            PeerAddr::Unix => self.unix,
        }
    }

    /// The client of a request that arrived from `peer`. From a trusted peer it is the
    /// rightmost untrusted hop of the configured header (every hop to its right was
    /// written by a trusted proxy); a hop that is not an address (`unknown`, an RFC 7239
    /// obfuscated `_id`) is keyed by its text, as that proxy reported it. When every hop
    /// is trusted, the leftmost of them; with no hop at all, the peer, which on the Unix
    /// socket (like an untrusted socket, or no peer) is one shared [`ClientKey::Unknown`].
    pub fn client_key(&self, peer: Option<PeerAddr>, headers: &HeaderMap) -> ClientKey {
        let mut client = match peer {
            Some(PeerAddr::Ip(ip)) => ClientKey::ip(ip),
            Some(PeerAddr::Unix) | None => ClientKey::Unknown,
        };
        match peer {
            Some(p) if self.trusts(p) => {}
            _ => return client,
        }
        for hop in forwarded_hops(headers, self.header).into_iter().rev() {
            match hop {
                Hop::Ip(ip) => {
                    client = ClientKey::ip(ip);
                    if !self.contains(ip) {
                        break;
                    }
                }
                Hop::Opaque(text) => {
                    client = ClientKey::Opaque(text.into());
                    break;
                }
            }
        }
        client
    }
}

/// IPv4-mapped IPv6 addresses as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// One hop of a forwarding header.
#[derive(Debug, PartialEq)]
enum Hop {
    Ip(IpAddr),
    /// not an address: its text (at most 64 bytes)
    Opaque(String),
}

/// The hops of `header` (for `Forwarded`, the `for=` of each element), left (origin)
/// to right (nearest proxy).
fn forwarded_hops(headers: &HeaderMap, header: ForwardHeader) -> Vec<Hop> {
    let parse = |s: &str| -> Hop {
        let s = s.trim().trim_matches('"');
        let ip = s.parse().ok().or_else(|| match s.strip_prefix('[') {
            // [v6]:port
            Some(rest) => rest.split(']').next()?.parse().ok(),
            // v4:port
            None => s.parse::<SocketAddr>().ok().map(|a| a.ip()),
        });
        match ip {
            Some(ip) => Hop::Ip(ip),
            None => {
                let mut end = s.len().min(64);
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                Hop::Opaque(s[..end].to_string())
            }
        }
    };
    let name = match header {
        ForwardHeader::XForwardedFor => "x-forwarded-for",
        ForwardHeader::Forwarded => "forwarded",
    };
    let elements = headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','));
    match header {
        ForwardHeader::XForwardedFor => elements.map(parse).collect(),
        ForwardHeader::Forwarded => elements
            .map(|elem| {
                let f = elem.split(';').find_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    k.trim().eq_ignore_ascii_case("for").then_some(v)
                });
                // an element without `for=` names no client
                parse(f.unwrap_or("unknown"))
            })
            .collect(),
    }
}

/// Whom a request is counted against.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// an address (IPv4, or the /64 of an IPv6 address)
    Ip(u128),
    /// an authenticated principal (or another key a named limit counts by)
    Principal(Arc<str>),
    /// a client a trusted proxy reported by a name rather than an address
    Opaque(Arc<str>),
    /// no address known (the Unix socket without a trusted proxy, in-process requests):
    /// one shared key
    Unknown,
}

impl ClientKey {
    pub fn ip(ip: IpAddr) -> ClientKey {
        match canonical(ip) {
            IpAddr::V4(v4) => ClientKey::Ip(u128::from(v4.to_ipv6_mapped())),
            IpAddr::V6(v6) => ClientKey::Ip(u128::from(v6) & !u128::from(u64::MAX)),
        }
    }

    pub fn principal(name: &str) -> ClientKey {
        ClientKey::Principal(name.into())
    }
}

/// Decides whom a request is counted against. The default, [`PeerKeyer`], uses the
/// client address; a deployment with authentication can key by principal instead
/// (falling back to the address for anonymous requests and for the `auth` class).
pub trait ClientKeyer: Send + Sync + 'static {
    fn key(&self, class: Class, req: &Request, trusted: &TrustedProxies) -> ClientKey;
}

/// The peer address (from `ConnectInfo`), or the forwarded client address when the
/// peer is a trusted proxy.
pub struct PeerKeyer;

impl ClientKeyer for PeerKeyer {
    fn key(&self, _: Class, req: &Request, trusted: &TrustedProxies) -> ClientKey {
        trusted.client_key(peer_addr(req), req.headers())
    }
}

/// The client a request is counted against by address, in the request extensions:
/// [`admit`] puts it there (with or without a limiter) for the auth layer's own limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientAddr(pub ClientKey);

#[cfg(feature = "otel")]
/// The address the connection came from, when the server recorded it.
pub fn peer_ip(req: &Request) -> Option<IpAddr> {
    match peer_addr(req)? {
        PeerAddr::Ip(ip) => Some(ip),
        PeerAddr::Unix => None,
    }
}

/// Where the connection came from, when the server recorded it.
pub fn peer_addr(req: &Request) -> Option<PeerAddr> {
    let ext = req.extensions();
    if let Some(c) = ext.get::<ConnectInfo<SocketAddr>>() {
        return Some(PeerAddr::Ip(c.0.ip()));
    }
    match ext.get::<ConnectInfo<crate::auth::Peer>>()? {
        ConnectInfo(crate::auth::Peer::Tcp(a)) => Some(PeerAddr::Ip(a.ip())),
        ConnectInfo(crate::auth::Peer::Unix) => Some(PeerAddr::Unix),
    }
}

// ------------------------------------------------------------------- clock ------

/// Monotonic nanoseconds; manual in tests.
#[derive(Clone)]
pub enum Clock {
    System(Instant),
    #[cfg_attr(not(test), allow(dead_code))]
    Manual(Arc<AtomicU64>),
}

/// Offset of the clock origin: a zero TAT always lies in the past.
const ORIGIN: u64 = 1 << 40;

impl Clock {
    fn now(&self) -> u64 {
        ORIGIN
            + match self {
                Clock::System(base) => {
                    u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX >> 2)
                }
                Clock::Manual(t) => t.load(Ordering::Relaxed),
            }
    }
}

// --------------------------------------------------------------------- GCRA ------

/// One policy: a class, optionally on one dataset, or a named limit.
#[derive(Clone)]
struct Policy {
    class: Class,
    /// `query`, `query@ds`, `preauth` or a named limit's name, for the headers; a reload
    /// keeps the state of a policy whose name stays
    name: String,
    /// what a named limit counts, for its error message (`tokens minted`)
    what: Option<&'static str>,
    /// the key of its client state ([`Registry`])
    id: u32,
    /// nanoseconds between requests at the sustained rate, and the burst
    gcra: Option<(u64, u32)>,
    rate: Option<Rate>,
    concurrency: Option<u32>,
    client_concurrency: Option<u32>,
    failure_cost: u32,
    /// requests in flight server-wide (the same counter under every configuration)
    inflight: Arc<AtomicU32>,
}

impl Policy {
    fn new(class: Class, name: String, (id, inflight): (u32, Arc<AtomicU32>), l: &Limit) -> Policy {
        let gcra = l.rate.map(|r| {
            let t = u64::try_from(r.period.as_nanos()).unwrap_or(u64::MAX) / u64::from(r.count);
            (t.max(1), l.burst.unwrap_or(r.count))
        });
        Policy {
            class,
            name,
            what: None,
            id,
            gcra,
            rate: l.rate,
            concurrency: l.concurrency,
            client_concurrency: l.client_concurrency,
            failure_cost: l.failure_cost.unwrap_or(1),
            inflight,
        }
    }

    fn needs_slot(&self) -> bool {
        self.gcra.is_some() || self.client_concurrency.is_some()
    }
}

/// A client's state under one policy.
#[derive(Default)]
struct Slot {
    /// theoretical arrival time of the next request (GCRA), in clock nanoseconds
    tat: AtomicU64,
    inflight: AtomicU32,
}

/// A client's standing: tokens left and seconds until full.
struct Admitted {
    remaining: u64,
    reset_secs: u64,
}

impl Slot {
    fn with_tat(tat: u64) -> Slot {
        Slot {
            tat: AtomicU64::new(tat),
            inflight: AtomicU32::new(0),
        }
    }

    /// Charge `cost` requests; `Err(wait)` when over the limit (nothing is charged).
    fn acquire(&self, now: u64, (t, burst): (u64, u32), cost: u32) -> Result<Admitted, u64> {
        let tau = t.saturating_mul(u64::from(burst));
        let inc = t.saturating_mul(u64::from(cost));
        let mut cur = self.tat.load(Ordering::Relaxed);
        loop {
            let tat = cur.max(now);
            let new = tat.saturating_add(inc);
            if new - now > tau {
                return Err(new - now - tau);
            }
            match self
                .tat
                .compare_exchange_weak(cur, new, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    let debt = new - now;
                    return Ok(Admitted {
                        remaining: (tau - debt) / t,
                        reset_secs: debt.div_ceil(1_000_000_000),
                    });
                }
                Err(x) => cur = x,
            }
        }
    }

    #[cfg(any(feature = "auth", test))]
    /// Whether `cost` more requests would be admitted now; `Err(wait)` when not (nothing
    /// is charged either way).
    fn check(&self, now: u64, (t, burst): (u64, u32), cost: u32) -> Result<(), u64> {
        let tau = t.saturating_mul(u64::from(burst));
        let new = self
            .tat
            .load(Ordering::Relaxed)
            .max(now)
            .saturating_add(t.saturating_mul(u64::from(cost)));
        if new - now > tau {
            Err(new - now - tau)
        } else {
            Ok(())
        }
    }

    /// Charge without a check (failed-auth weighting).
    fn charge(&self, now: u64, t: u64, cost: u32) {
        let inc = t.saturating_mul(u64::from(cost));
        let _ = self
            .tat
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.max(now).saturating_add(inc))
            });
    }

    /// Give back a charge that turned out not to be owed (a reservation).
    fn refund(&self, t: u64, cost: u32) {
        let dec = t.saturating_mul(u64::from(cost));
        let _ = self
            .tat
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.saturating_sub(dec))
            });
    }

    fn standing(&self, now: u64, (t, burst): (u64, u32)) -> Admitted {
        let debt = self.tat.load(Ordering::Relaxed).saturating_sub(now);
        Admitted {
            remaining: t.saturating_mul(u64::from(burst)).saturating_sub(debt) / t,
            reset_secs: debt.div_ceil(1_000_000_000),
        }
    }

    fn idle(&self, now: u64) -> bool {
        self.tat.load(Ordering::Relaxed) <= now && self.inflight.load(Ordering::Relaxed) == 0
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SlotKey {
    policy: u32,
    client: ClientKey,
}

/// Debts of evicted clients: the TAT they left behind.
type Penalties = quick_cache::sync::Cache<SlotKey, u64>;

/// Clients with requests in flight are never evicted; an evicted client that still owes
/// time leaves its TAT in the penalty cache, where its next request finds it.
#[derive(Clone)]
struct Evict {
    clock: Clock,
    penalties: Arc<Penalties>,
    evictions: Arc<AtomicU64>,
}

impl quick_cache::Lifecycle<SlotKey, Arc<Slot>> for Evict {
    type RequestState = ();
    fn is_pinned(&self, _: &SlotKey, v: &Arc<Slot>) -> bool {
        v.inflight.load(Ordering::Relaxed) > 0
    }
    fn on_evict(&self, _: &mut (), key: SlotKey, v: Arc<Slot>) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        let tat = v.tat.load(Ordering::Relaxed);
        if tat > self.clock.now() {
            self.penalties.insert(key, tat);
        }
    }
}

type Slots = quick_cache::sync::Cache<
    SlotKey,
    Arc<Slot>,
    quick_cache::UnitWeighter,
    quick_cache::DefaultHashBuilder,
    Evict,
>;

/// The client state of a limiter's policies: slots in a bounded cache, and the debts of
/// evicted clients in a cache an eighth of its size.
struct Buckets {
    slots: Slots,
    penalties: Arc<Penalties>,
    evictions: Arc<AtomicU64>,
    max_keys: usize,
}

impl Buckets {
    fn new(max_keys: usize, clock: &Clock, evictions: Arc<AtomicU64>) -> Result<Buckets, String> {
        let penalties = Arc::new(Penalties::new((max_keys / 8).max(16)));
        let slots = Slots::with_options(
            quick_cache::OptionsBuilder::new()
                .estimated_items_capacity(max_keys)
                .weight_capacity(max_keys as u64)
                .build()
                .map_err(|e| format!("{e:?}"))?,
            quick_cache::UnitWeighter,
            Default::default(),
            Evict {
                clock: clock.clone(),
                penalties: penalties.clone(),
                evictions: evictions.clone(),
            },
        );
        Ok(Buckets {
            slots,
            penalties,
            evictions,
            max_keys,
        })
    }

    #[cfg(any(feature = "auth", test))]
    /// A client's state when it has any (tracked, or a debt left at eviction); nothing is
    /// stored for a client that has none.
    fn get(&self, key: &SlotKey) -> Option<Arc<Slot>> {
        self.slots.get(key).or_else(|| {
            self.penalties
                .get(key)
                .map(|tat| Arc::new(Slot::with_tat(tat)))
        })
    }

    /// A client's state, created (with the debt it left at eviction) when not tracked.
    fn slot(&self, key: SlotKey) -> Arc<Slot> {
        match self.slots.get_or_insert_with(&key, || {
            let tat = self.penalties.remove(&key).map_or(0, |(_, t)| t);
            Ok::<_, ()>(Arc::new(Slot::with_tat(tat)))
        }) {
            Ok(s) => s,
            Err(()) => unreachable!(),
        }
    }

    /// Drop idle clients and paid debts; returns how many clients were dropped.
    fn sweep(&self, now: u64) -> usize {
        let before = self.slots.len();
        self.slots.retain(|_, s| !s.idle(now));
        self.penalties.retain(|_, tat| *tat > now);
        before.saturating_sub(self.slots.len())
    }

    /// The same clients in caches of another size (a reload that changed `maxKeys`).
    fn resized(&self, max_keys: usize, clock: &Clock) -> Result<Buckets, String> {
        let b = Buckets::new(max_keys, clock, self.evictions.clone())?;
        for (k, tat) in self.penalties.iter() {
            b.penalties.insert(k, tat);
        }
        for (k, s) in self.slots.iter() {
            b.slots.insert(k, s);
        }
        Ok(b)
    }
}

/// Policy ids and server-wide in-flight counters by policy name, kept across reloads.
#[derive(Default)]
struct Registry(parking_lot::Mutex<HashMap<String, (u32, Arc<AtomicU32>)>>);

impl Registry {
    fn get(&self, name: &str) -> (u32, Arc<AtomicU32>) {
        let mut m = self.0.lock();
        let next = m.len() as u32;
        m.entry(name.to_string())
            .or_insert_with(|| (next, Arc::default()))
            .clone()
    }
}

/// One configuration's compiled policies (replaced on reload; the client state and the
/// in-flight counters carry over).
#[derive(Clone)]
struct Inner {
    policies: Vec<Policy>,
    by_class: [Option<u32>; Class::COUNT],
    by_dataset: BTreeMap<String, [Option<u32>; Class::COUNT]>,
    #[cfg(any(feature = "auth", test))]
    named: HashMap<&'static str, u32>,
    /// the `preauth` limit of an IPv6 /48 ([`AGGREGATE_FACTOR`])
    preauth_aggregate: Option<u32>,
    trusted: TrustedProxies,
    buckets: Arc<Buckets>,
}

pub const DEFAULT_MAX_KEYS: usize = 100_000;

impl Inner {
    fn new(
        cfg: &Config,
        reg: &Registry,
        prev: Option<&Inner>,
        clock: &Clock,
        evictions: &Arc<AtomicU64>,
    ) -> Result<Inner, String> {
        let mut policies = Vec::new();
        let mut add = |class: Class, name: String, l: &Limit| -> Option<u32> {
            if l.is_unlimited() {
                return None;
            }
            let ids = reg.get(&name);
            policies.push(Policy::new(class, name, ids, l));
            Some((policies.len() - 1) as u32)
        };
        let mut by_class = [None; Class::COUNT];
        for (c, l) in &cfg.classes {
            let class = Class::parse(c).ok_or_else(|| format!("unknown class '{c}'"))?;
            by_class[class.index()] = add(class, class.as_str().to_string(), l);
        }
        let preauth_aggregate = match cfg.classes.get(Class::PreAuth.as_str()) {
            Some(l) => add(Class::PreAuth, "preauth/48".into(), &aggregate_limit(l)),
            None => None,
        };
        let mut by_dataset = BTreeMap::new();
        for (ds, m) in &cfg.datasets {
            // a dataset override for one class leaves the others at the class limit
            let mut per = by_class;
            for (c, l) in m {
                let class = Class::parse(c).ok_or_else(|| format!("unknown class '{c}'"))?;
                per[class.index()] = add(class, format!("{}@{ds}", class.as_str()), l);
            }
            by_dataset.insert(ds.clone(), per);
        }
        // named limits are charged by the auth layer only
        #[cfg(any(feature = "auth", test))]
        let named = {
            let mut named = HashMap::new();
            for (name, (l, _)) in &cfg.named {
                if let Some(i) = add(Class::Auth, name.to_string(), l) {
                    named.insert(*name, i);
                }
            }
            for (name, (_, what)) in &cfg.named {
                if let Some(&i) = named.get(name) {
                    policies[i as usize].what = Some(*what);
                }
            }
            named
        };
        let max_keys = cfg.max_keys.unwrap_or(DEFAULT_MAX_KEYS).max(16);
        let buckets = match prev {
            Some(p) if p.buckets.max_keys == max_keys => p.buckets.clone(),
            Some(p) => Arc::new(p.buckets.resized(max_keys, clock)?),
            None => Arc::new(Buckets::new(max_keys, clock, evictions.clone())?),
        };
        Ok(Inner {
            policies,
            by_class,
            by_dataset,
            #[cfg(any(feature = "auth", test))]
            named,
            preauth_aggregate,
            trusted: cfg.trusted()?,
            buckets,
        })
    }

    fn policy(&self, class: Class, dataset: Option<&str>) -> Option<u32> {
        dataset
            .and_then(|d| self.by_dataset.get(d))
            .map_or(self.by_class[class.index()], |p| p[class.index()])
    }

    #[cfg(any(feature = "auth", test))]
    /// A named limit with a rate.
    fn named(&self, name: &str) -> Option<(&Policy, (u64, u32))> {
        let p = &self.policies[*self.named.get(name)? as usize];
        Some((p, p.gcra?))
    }
}

// ---------------------------------------------------------------- the limiter ------

/// Rate and concurrency limits of a server; shared by every request.
pub struct RateLimiter {
    inner: ArcSwap<Inner>,
    registry: Registry,
    evictions: Arc<AtomicU64>,
    clock: Clock,
    keyer: Arc<dyn ClientKeyer>,
    /// requests with a forwarding header from a peer that is not a trusted proxy
    untrusted_forwarded: AtomicU64,
    forwarded_warned: AtomicBool,
}

/// The size of a limiter's client state (the `sparkles_rate_limit_*` metrics).
pub struct Stats {
    /// clients tracked, over all policies
    pub keys: u64,
    pub max_keys: u64,
    /// clients evicted to stay within `max_keys`
    pub evictions: u64,
    /// evicted clients whose debt is remembered
    pub penalties: u64,
    /// requests with a forwarding header from an untrusted peer
    pub untrusted_forwarded: u64,
}

impl RateLimiter {
    pub fn new(cfg: &Config) -> Result<RateLimiter, String> {
        let registry = Registry::default();
        let evictions = Arc::default();
        let clock = Clock::System(Instant::now());
        let inner = Inner::new(cfg, &registry, None, &clock, &evictions)?;
        Ok(RateLimiter {
            inner: ArcSwap::from_pointee(inner),
            registry,
            evictions,
            clock,
            keyer: Arc::new(PeerKeyer),
            untrusted_forwarded: AtomicU64::new(0),
            forwarded_warned: AtomicBool::new(false),
        })
    }

    /// Replace the clock (tests); the client state starts afresh.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_clock(mut self, clock: Clock) -> RateLimiter {
        let mut inner = Inner::clone(&self.inner.load());
        if let Ok(b) = Buckets::new(inner.buckets.max_keys, &clock, self.evictions.clone()) {
            inner.buckets = Arc::new(b);
        }
        self.inner = ArcSwap::from_pointee(inner);
        self.clock = clock;
        self
    }

    /// Replace how clients are keyed (e.g. by authenticated principal).
    pub fn with_keyer(mut self, keyer: Arc<dyn ClientKeyer>) -> RateLimiter {
        self.keyer = keyer;
        self
    }

    /// Switch to a new configuration. A policy whose name stays keeps its client state:
    /// debts are not forgiven, and requests already in flight count against the new
    /// concurrency caps (a lower cap admits nothing until they drop below it). Requests
    /// in flight finish under the configuration they started with.
    pub fn reload(&self, cfg: &Config) -> Result<(), String> {
        let prev = self.inner.load_full();
        let next = Inner::new(
            cfg,
            &self.registry,
            Some(&prev),
            &self.clock,
            &self.evictions,
        )?;
        self.inner.store(Arc::new(next));
        Ok(())
    }

    /// Drop the state of idle clients (bucket full again, nothing in flight) and paid
    /// debts; returns how many clients were dropped.
    pub fn sweep(&self) -> usize {
        self.inner.load().buckets.sweep(self.clock.now())
    }

    /// Clients currently tracked (over all policies).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn tracked(&self) -> usize {
        self.inner.load().buckets.slots.len()
    }

    pub fn stats(&self) -> Stats {
        let inner = self.inner.load();
        let b = &inner.buckets;
        Stats {
            keys: b.slots.len() as u64,
            max_keys: b.max_keys as u64,
            evictions: b.evictions.load(Ordering::Relaxed),
            penalties: b.penalties.len() as u64,
            untrusted_forwarded: self.untrusted_forwarded.load(Ordering::Relaxed),
        }
    }

    /// Count a forwarding header from a peer that is not a trusted proxy, and warn about
    /// the first: the server probably runs behind a proxy it does not trust, so every
    /// client counts as the proxy.
    fn note_untrusted_forwarding(&self, req: &Request, trusted: &TrustedProxies) {
        let h = req.headers();
        if !h.contains_key("x-forwarded-for") && !h.contains_key(header::FORWARDED) {
            return;
        }
        let Some(peer) = peer_addr(req) else { return };
        if trusted.trusts(peer) {
            return;
        }
        self.untrusted_forwarded.fetch_add(1, Ordering::Relaxed);
        if !self.forwarded_warned.swap(true, Ordering::Relaxed) {
            let from = match peer {
                PeerAddr::Ip(ip) => ip.to_string(),
                PeerAddr::Unix => "the Unix socket".to_string(),
            };
            tracing::warn!(
                "a request from {from} carries X-Forwarded-For or Forwarded, but {from} is not a \
                 trusted proxy: the header is ignored. Behind a reverse proxy every client then \
                 counts as the proxy and shares its budgets; list the proxy with \
                 --rate-limit-trusted-proxy (warned once; sparkles_rate_limit_untrusted_forwarded_total \
                 counts them)"
            );
        }
    }

    #[cfg(any(feature = "auth", test))]
    /// Charge `cost` to `client` under the named limit `name` (nothing to do when it is
    /// not configured); over the limit, nothing is charged and the `429` to answer is
    /// returned.
    #[allow(clippy::result_large_err)]
    pub fn acquire(
        &self,
        name: &'static str,
        client: ClientKey,
        cost: u32,
    ) -> Result<(), Response> {
        let inner = self.inner.load();
        let Some((p, g)) = inner.named(name) else {
            return Ok(());
        };
        let slot = inner.buckets.slot(SlotKey {
            policy: p.id,
            client,
        });
        match slot.acquire(self.clock.now(), g, cost) {
            Ok(_) => Ok(()),
            Err(wait) => Err(reject(
                p,
                StatusCode::TOO_MANY_REQUESTS,
                secs_ceil(wait),
                name,
                0,
            )),
        }
    }

    #[cfg(any(feature = "auth", test))]
    /// Whether the named limit admits one more for `client`, without charging it.
    #[allow(clippy::result_large_err)]
    pub fn check(&self, name: &'static str, client: &ClientKey) -> Result<(), Response> {
        let inner = self.inner.load();
        let Some((p, g)) = inner.named(name) else {
            return Ok(());
        };
        let key = SlotKey {
            policy: p.id,
            client: client.clone(),
        };
        match inner
            .buckets
            .get(&key)
            .map(|s| s.check(self.clock.now(), g, 1))
        {
            Some(Err(wait)) => Err(reject(
                p,
                StatusCode::TOO_MANY_REQUESTS,
                secs_ceil(wait),
                name,
                0,
            )),
            _ => Ok(()),
        }
    }

    #[cfg(any(feature = "auth", test))]
    /// Charge `cost` to `client` under the named limit without a check (a failure found
    /// after the fact; [`RateLimiter::check`] refuses the next request).
    pub fn charge(&self, name: &'static str, client: ClientKey, cost: u32) {
        let inner = self.inner.load();
        if let Some((p, (t, _))) = inner.named(name) {
            inner
                .buckets
                .slot(SlotKey {
                    policy: p.id,
                    client,
                })
                .charge(self.clock.now(), t, cost);
        }
    }
}

/// The `sparkles_rate_limit_*` families of some limiters (label `limiter`), in the
/// Prometheus text format.
pub fn render_metrics(o: &mut String, limiters: &[(&str, &RateLimiter)]) {
    use std::fmt::Write;
    type Field = fn(&Stats) -> u64;
    let stats: Vec<(&str, Stats)> = limiters.iter().map(|(l, rl)| (*l, rl.stats())).collect();
    let families: [(&str, &str, &str, Field); 5] = [
        (
            "sparkles_rate_limit_keys",
            "gauge",
            "Clients a rate limiter tracks, over all its policies.",
            |s| s.keys,
        ),
        (
            "sparkles_rate_limit_max_keys",
            "gauge",
            "Clients a rate limiter tracks at most (maxKeys).",
            |s| s.max_keys,
        ),
        (
            "sparkles_rate_limit_evictions_total",
            "counter",
            "Tracked clients evicted to stay within maxKeys.",
            |s| s.evictions,
        ),
        (
            "sparkles_rate_limit_penalties",
            "gauge",
            "Evicted clients whose unpaid debt is remembered.",
            |s| s.penalties,
        ),
        (
            "sparkles_rate_limit_untrusted_forwarded_total",
            "counter",
            "Requests with X-Forwarded-For or Forwarded from a peer that is not a trusted proxy (ignored).",
            |s| s.untrusted_forwarded,
        ),
    ];
    for (name, kind, help, f) in families {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
        for (l, s) in &stats {
            let _ = writeln!(o, "{name}{{limiter=\"{l}\"}} {}", f(s));
        }
    }
}

/// Where a server's rate-limit configuration comes from: `--rate-limit-config`, then
/// `--rate-limit`, `--rate-limit-trusted-proxy` and `--rate-limit-trusted-proxy-header`
/// on top.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub file: Option<std::path::PathBuf>,
    pub flags: Vec<String>,
    pub trusted_proxies: Vec<String>,
    pub trusted_proxy_header: Option<String>,
    /// authentication is on: `preauth` defaults to [`DEFAULT_PREAUTH`]
    pub auth: bool,
}

impl Sources {
    /// The merged configuration; `None` when nothing is configured and there is no
    /// file (no limiter at all, so no overhead). Trusted proxies alone make a limiter
    /// without limits: it still names the client the auth layer's own limits count.
    pub fn load(&self) -> anyhow::Result<Option<Config>> {
        let mut cfg = match &self.file {
            Some(f) => Config::read(f)?,
            None => Config::default(),
        };
        for f in &self.flags {
            cfg.apply_flag(f).map_err(anyhow::Error::msg)?;
        }
        cfg.trusted_proxies
            .extend(self.trusted_proxies.iter().cloned());
        if let Some(h) = &self.trusted_proxy_header {
            cfg.trusted_proxy_header = Some(h.clone());
        }
        let preauth = Class::PreAuth.as_str();
        if self.auth && !cfg.classes.contains_key(preauth) {
            let l = Limit::parse(DEFAULT_PREAUTH).map_err(anyhow::Error::msg)?;
            cfg.classes.insert(preauth.to_string(), l);
        }
        cfg.validate()?;
        let any = self.file.is_some() || !cfg.is_empty() || !cfg.trusted_proxies.is_empty();
        Ok(any.then_some(cfg))
    }
}

/// Startup warnings about telling clients apart. With authentication on, a server that
/// listens where a reverse proxy usually connects from (the Unix socket, a loopback
/// address) and trusts no proxy there counts every client as the proxy: one client's
/// failed logins then spend everybody's budget.
pub fn client_warnings(
    cfg: Option<&Config>,
    auth: bool,
    unix_socket: bool,
    loopback: bool,
) -> Vec<String> {
    if !auth {
        return Vec::new();
    }
    let trusted = cfg.and_then(|c| c.trusted().ok()).unwrap_or_default();
    let mut w = Vec::new();
    if unix_socket && !trusted.unix() {
        w.push(
            "authentication is on and the Unix socket trusts no proxy: every client of the \
             socket shares one budget of failed authentications and device logins, so one \
             client's bad passwords refuse everybody's password logins for a while. If a \
             reverse proxy connects through the socket, pass --rate-limit-trusted-proxy unix \
             (the proxy must overwrite X-Forwarded-For)"
                .to_string(),
        );
    }
    if !unix_socket && loopback && trusted.nets.is_empty() {
        w.push(
            "authentication is on and the server listens on loopback with no trusted proxy: \
             behind a reverse proxy every client counts as the proxy and shares one budget of \
             failed authentications and device logins. Pass --rate-limit-trusted-proxy \
             127.0.0.1 --rate-limit-trusted-proxy ::1 (the proxy must overwrite \
             X-Forwarded-For)"
                .to_string(),
        );
    }
    w
}
/// Re-read the configuration on SIGHUP; a bad file keeps the running configuration.
#[cfg(unix)]
pub fn spawn_reload_on_sighup(rl: Arc<RateLimiter>, sources: Sources) {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut hup) = signal(SignalKind::hangup()) else {
        return;
    };
    tokio::spawn(async move {
        while hup.recv().await.is_some() {
            let s = sources.clone();
            let loaded = tokio::task::spawn_blocking(move || s.load()).await;
            match loaded {
                Ok(Ok(cfg)) => match rl.reload(&cfg.unwrap_or_default()) {
                    Ok(()) => tracing::info!("rate limits reloaded"),
                    Err(e) => tracing::error!("rate limits not reloaded: {e}"),
                },
                Ok(Err(e)) => tracing::error!("rate limits not reloaded: {e:#}"),
                Err(e) => tracing::error!("rate limits not reloaded: {e}"),
            }
        }
    });
}

/// Sweep idle client state every `every` (on the current tokio runtime).
pub fn spawn_sweeper(rl: Arc<RateLimiter>, every: Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.tick().await;
        loop {
            tick.tick().await;
            let n = rl.sweep();
            if n > 0 {
                tracing::debug!(dropped = n, "rate limiter: idle clients dropped");
            }
        }
    });
}

/// Requests in flight counted against a server-wide and a per-client cap, released
/// when the response body is done and the work started for the request has ended
/// (see [`hold`]).
struct Permits {
    server: Option<Arc<AtomicU32>>,
    client: Option<Arc<Slot>>,
}

impl Drop for Permits {
    fn drop(&mut self) {
        if let Some(n) = &self.server {
            n.fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(s) = &self.client {
            s.inflight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

tokio::task_local! {
    /// The permits of the request whose handler runs in this task.
    static HELD: Arc<Permits>;
}

/// A share of the current request's concurrency permits (nothing when no cap applies):
/// work that can outlive the request's future (a blocking task keeps running after the
/// client disconnects) keeps it until the work ends, so the caps count the work, not
/// the connection.
pub struct Held(#[allow(dead_code)] Option<Arc<Permits>>);

/// Take a share of the current request's permits (see [`Held`]).
pub fn hold() -> Held {
    Held(HELD.try_with(Arc::clone).ok())
}

/// A response body that holds the request's permits until it is sent (streamed Graph
/// Store GETs keep working after the handler returned).
struct PermitBody {
    inner: Body,
    _permits: Arc<Permits>,
}

impl HttpBody for PermitBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

fn secs_ceil(nanos: u64) -> u64 {
    nanos.div_ceil(1_000_000_000).max(1)
}

/// Add a member to a structured-field list header: a response may carry the policies of
/// both stages.
fn put_member(h: &mut HeaderMap, name: &'static str, member: String) {
    let v = match h.get(name).and_then(|v| v.to_str().ok()) {
        Some(old) => format!("{old}, {member}"),
        None => member,
    };
    if let Ok(v) = HeaderValue::from_str(&v) {
        h.insert(name, v);
    }
}

/// `RateLimit-Policy: "query";q=100;w=1` and `RateLimit: "query";r=57;t=1`
/// (draft-ietf-httpapi-ratelimit-headers-11).
fn rate_headers(h: &mut HeaderMap, p: &Policy, remaining: u64, reset: u64) {
    let Some(rate) = p.rate else { return };
    let policy = format!(
        "\"{}\";q={};w={}",
        p.name,
        rate.count,
        rate.period.as_secs().max(1)
    );
    put_member(h, "ratelimit-policy", policy);
    put_member(
        h,
        "ratelimit",
        format!("\"{}\";r={remaining};t={reset}", p.name),
    );
}

fn reject(
    p: &Policy,
    status: StatusCode,
    retry: u64,
    reason: &'static str,
    remaining: u64,
) -> Response {
    let class = p.class.as_str();
    let msg = if let Some(what) = p.what {
        format!("too many {what}: retry in {retry} s")
    } else if p.class == Class::PreAuth {
        format!("too many failed authentications: retry in {retry} s")
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        format!("too many {class} requests: retry in {retry} s")
    } else {
        format!("too many concurrent {class} requests: retry in {retry} s")
    };
    crate::otel::rate_limited(class, reason);
    let mut resp = (
        status,
        [(header::RETRY_AFTER, HeaderValue::from(retry))],
        axum::Json(json!({
            "error": msg,
            "limitClass": class,
            "reason": reason,
            "retryAfterSeconds": retry,
        })),
    )
        .into_response();
    rate_headers(resp.headers_mut(), p, remaining, retry);
    RequestReport {
        outcome: Some(Outcome::RateLimited),
        limit_class: Some(p.class),
        ..Default::default()
    }
    .attach(resp)
}

// ------------------------------------------------------ before authentication ------

/// Marks a response as a failed credential check, the only thing the `preauth` limit
/// charges: a wrong password, an unknown, expired or malformed token, an invalid session
/// cookie, an unknown device code, a CSRF refusal on an auth route. The auth layer and
/// its handlers set it; refusals of authorization (a `403`, a hidden dataset's `404`),
/// handler errors and a busy password check are not failures of the address.
#[derive(Clone, Copy, Debug)]
pub struct AuthFailed;

/// An IPv6 /48 (a site) has this many times the `preauth` budget of one of its /64s,
/// on top of theirs: a network that holds many /64s cannot multiply its budget.
pub const AGGREGATE_FACTOR: u32 = 8;

/// The `preauth` limit of a /48: the /64 limit times [`AGGREGATE_FACTOR`].
fn aggregate_limit(l: &Limit) -> Limit {
    Limit {
        rate: l.rate.map(|r| Rate {
            count: r.count.saturating_mul(AGGREGATE_FACTOR),
            period: r.period,
        }),
        burst: l
            .burst
            .or(l.rate.map(|r| r.count))
            .map(|b| b.saturating_mul(AGGREGATE_FACTOR)),
        ..l.clone()
    }
}

impl ClientKey {
    #[cfg(any(feature = "auth", test))]
    /// The network a client belongs to: the /48 of an IPv6 address; an IPv4 address and
    /// any other key are their own.
    pub fn network(&self) -> ClientKey {
        self.ipv6_network().unwrap_or_else(|| self.clone())
    }

    /// The /48 of an IPv6 address.
    fn ipv6_network(&self) -> Option<ClientKey> {
        match self {
            ClientKey::Ip(a) if !is_mapped_v4(*a) => Some(ClientKey::Ip(a & !((1u128 << 80) - 1))),
            _ => None,
        }
    }
}

/// Whether a [`ClientKey::Ip`] value is an IPv4 address (mapped into IPv6).
fn is_mapped_v4(a: u128) -> bool {
    a >> 32 == 0xffff
}

/// A request's standing under the `preauth` limit (in the request extensions): the auth
/// layer reserves the cost of a failure before it verifies a password, so concurrent
/// guesses cannot all start hashing before the first of them has failed.
#[derive(Clone)]
pub struct Admission(Arc<AdmissionState>);

struct AdmissionState {
    inner: Arc<Inner>,
    /// the client's policy and key, then its /48's (IPv6 only)
    slots: Vec<(u32, ClientKey)>,
    clock: Clock,
    reserved: AtomicU32,
}

impl Admission {
    fn new(inner: Arc<Inner>, client: ClientKey, clock: Clock) -> Option<Admission> {
        let p = inner.by_class[Class::PreAuth.index()]?;
        let mut slots = vec![(p, client.clone())];
        if let (Some(a), Some(net)) = (inner.preauth_aggregate, client.ipv6_network()) {
            slots.push((a, net));
        }
        Some(Admission(Arc::new(AdmissionState {
            inner,
            slots,
            clock,
            reserved: AtomicU32::new(0),
        })))
    }

    /// The policies and client states the request is counted under (with a rate).
    fn each(&self) -> impl Iterator<Item = (&Policy, (u64, u32), SlotKey)> {
        let a = &self.0;
        a.slots.iter().filter_map(|(i, client)| {
            let p = &a.inner.policies[*i as usize];
            let key = SlotKey {
                policy: p.id,
                client: client.clone(),
            };
            Some((p, p.gcra?, key))
        })
    }

    fn cost(&self) -> u32 {
        self.0.inner.policies[self.0.slots[0].0 as usize].failure_cost
    }

    #[cfg(any(feature = "auth", test))]
    /// Charge a failure now, before an expensive credential check; it is given back when
    /// the request does not fail. `false`: the address (or its network) has no failures
    /// left (answer [`Admission::refusal`]).
    pub fn reserve(&self) -> bool {
        let a = &self.0;
        if a.reserved.load(Ordering::Relaxed) > 0 {
            return true;
        }
        let (now, cost) = (a.clock.now(), self.cost());
        let mut taken: Vec<(Arc<Slot>, u64)> = Vec::new();
        for (_, g, key) in self.each() {
            let slot = a.inner.buckets.slot(key);
            if slot.acquire(now, g, cost).is_err() {
                for (s, t) in taken {
                    s.refund(t, cost);
                }
                return false;
            }
            taken.push((slot, g.0));
        }
        a.reserved.store(cost, Ordering::Relaxed);
        true
    }

    #[cfg(feature = "auth")]
    /// Whether the address (or its network) has no failures left for a request that
    /// has not reserved one: its password checks and unknown tokens are refused.
    pub fn exhausted(&self) -> bool {
        if self.0.reserved.load(Ordering::Relaxed) > 0 {
            return false;
        }
        let (now, cost) = (self.0.clock.now(), self.cost());
        self.each().any(|(_, g, key)| {
            self.0
                .inner
                .buckets
                .get(&key)
                .is_some_and(|s| s.check(now, g, cost).is_err())
        })
    }

    #[cfg(any(feature = "auth", test))]
    /// The `429` of a request whose reservation failed, or of an exhausted address.
    pub fn refusal(&self) -> Response {
        let (now, cost) = (self.0.clock.now(), self.cost());
        let mut worst: Option<(&Policy, u64)> = None;
        for (p, g, key) in self.each() {
            let wait = self
                .0
                .inner
                .buckets
                .get(&key)
                .and_then(|s| s.check(now, g, cost).err())
                .unwrap_or(0);
            if worst.is_none_or(|(_, w)| wait > w) {
                worst = Some((p, wait));
            }
        }
        let a = &self.0;
        let (p, wait) = worst.unwrap_or((&a.inner.policies[a.slots[0].0 as usize], 0));
        reject(
            p,
            StatusCode::TOO_MANY_REQUESTS,
            secs_ceil(wait.max(1)),
            "failures",
            0,
        )
    }

    /// After the response: charge a failure (what the reservation did not already
    /// cover) and report the standing, or give the reservation back.
    fn settle(&self, resp: &mut Response) {
        let a = &self.0;
        let reserved = a.reserved.load(Ordering::Relaxed);
        let failed = resp.extensions().get::<AuthFailed>().is_some();
        if !failed && reserved == 0 {
            return;
        }
        let now = a.clock.now();
        let rest = self.cost().saturating_sub(reserved);
        for (i, (p, g, key)) in self.each().enumerate() {
            let slot = a.inner.buckets.slot(key);
            if !failed {
                if reserved > 0 {
                    slot.refund(g.0, reserved);
                }
                continue;
            }
            if rest > 0 {
                slot.charge(now, g.0, rest);
            }
            if i == 0 {
                let s = slot.standing(now, g);
                rate_headers(resp.headers_mut(), p, s.remaining, s.reset_secs);
            }
        }
    }
}

/// The first stage (`from_fn_with_state`, outside the auth layer and inside
/// `obs::observe` and CORS). It names the client ([`ClientAddr`], for the auth layer's
/// own limits too) and, with a `preauth` limit, puts the request's [`Admission`] into
/// it: a budget of failed credential checks per address (and IPv6 /48). Only failures
/// are charged (`failure-cost`, default 1), and only what would verify a password or
/// look up an unknown token is refused once the budget is spent (the auth layer asks
/// the admission); valid tokens and sessions, anonymous requests, health checks and the
/// UI pass. An address without failures costs a cache lookup; nothing is stored for it.
pub async fn admit(
    State(rl): State<Option<Arc<RateLimiter>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(rl) = rl else {
        // no limiter, so no trusted proxy either: the peer
        let client = PeerKeyer.key(Class::PreAuth, &req, &TrustedProxies::default());
        req.extensions_mut().insert(ClientAddr(client));
        return next.run(req).await;
    };
    let inner = rl.inner.load_full();
    rl.note_untrusted_forwarding(&req, &inner.trusted);
    // without an address (the Unix socket without a trusted proxy, in-process requests)
    // every such client shares one key: a shared budget still bounds password guessing
    let client = PeerKeyer.key(Class::PreAuth, &req, &inner.trusted);
    req.extensions_mut().insert(ClientAddr(client.clone()));
    let Some(adm) = Admission::new(inner, client, rl.clock.clone()) else {
        return next.run(req).await;
    };
    req.extensions_mut().insert(adm.clone());
    let mut resp = next.run(req).await;
    adm.settle(&mut resp);
    resp
}

// ------------------------------------------------------- after authentication ------

/// The second stage (`from_fn_with_state`, inside the auth layer, which puts the
/// principal in the request, and inside `obs::observe`, so limited requests are logged
/// and counted): the class limits.
pub async fn limit(State(rl): State<Arc<RateLimiter>>, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map(|m| m.as_str());
    let Some(class) = classify(route, req.method(), req.uri(), req.headers()) else {
        return next.run(req).await;
    };
    let inner = rl.inner.load_full();
    let dataset = dataset_of(route, req.uri());
    let Some(pi) = inner.policy(class, dataset.as_deref()) else {
        return next.run(req).await;
    };
    let p = &inner.policies[pi as usize];
    let slot = p.needs_slot().then(|| {
        inner.buckets.slot(SlotKey {
            policy: p.id,
            client: rl.keyer.key(class, &req, &inner.trusted),
        })
    });
    let mut permits = Permits {
        server: None,
        client: None,
    };
    // concurrency first: it is released on rejection, a rate token is not
    if let Some(max) = p.concurrency {
        let n = p.inflight.fetch_add(1, Ordering::Relaxed);
        permits.server = Some(p.inflight.clone());
        if n >= max {
            return reject(p, StatusCode::SERVICE_UNAVAILABLE, 1, "concurrency", 0);
        }
    }
    if let (Some(max), Some(s)) = (p.client_concurrency, &slot) {
        let n = s.inflight.fetch_add(1, Ordering::Relaxed);
        permits.client = Some(s.clone());
        if n >= max {
            return reject(
                p,
                StatusCode::SERVICE_UNAVAILABLE,
                1,
                "client-concurrency",
                0,
            );
        }
    }
    let mut admitted = None;
    if let (Some(g), Some(s)) = (p.gcra, &slot) {
        match s.acquire(rl.clock.now(), g, 1) {
            Ok(a) => admitted = Some(a),
            Err(wait) => {
                return reject(p, StatusCode::TOO_MANY_REQUESTS, secs_ceil(wait), "rate", 0);
            }
        }
    }
    let held = permits.server.is_some() || permits.client.is_some();
    let permits = Arc::new(permits);
    let mut resp = if held {
        HELD.scope(permits.clone(), next.run(req)).await
    } else {
        next.run(req).await
    };
    if let Some(a) = admitted {
        let (mut remaining, mut reset) = (a.remaining, a.reset_secs);
        if p.failure_cost > 1
            && matches!(
                resp.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            )
            && let (Some(g), Some(s)) = (p.gcra, &slot)
        {
            let now = rl.clock.now();
            s.charge(now, g.0, p.failure_cost - 1);
            let st = s.standing(now, g);
            (remaining, reset) = (st.remaining, st.reset_secs);
        }
        rate_headers(resp.headers_mut(), p, remaining, reset);
    }
    if held {
        resp = resp.map(|b| {
            Body::new(PermitBody {
                inner: b,
                _permits: permits,
            })
        });
    }
    resp
}

#[cfg(test)]
mod tests;
