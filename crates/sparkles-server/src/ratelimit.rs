//! Rate and concurrency limits per request class and client.
//!
//! Requests fall into four classes by their matched route and method ([`Class`]):
//! `auth` (everything under `/$/auth/`), `query`, `update` and `admin`; health checks,
//! `/$/metrics`, the UI and cheap admin reads belong to none and are never limited. Each
//! class (optionally overridden per dataset) has a [`Limit`]: a request rate with a burst,
//! enforced per client with GCRA (the "virtual scheduling" form of a token bucket, one
//! `u64` per client), and caps on the requests in flight, server-wide and per client.
//!
//! * Over the rate: `429 Too Many Requests` with `Retry-After`.
//! * Over a concurrency cap: `503 Service Unavailable` with `Retry-After: 1`, at once
//!   (requests are never queued, so a saturated server sheds load instead of piling up
//!   waiting requests and their memory).
//!
//! Responses of rate-limited classes carry `RateLimit-Policy` and `RateLimit` in the
//! structured-field syntax of draft-ietf-httpapi-ratelimit-headers-11.
//!
//! Clients are keyed by [`ClientKeyer`]: the peer address by default (IPv6 by its /64),
//! or the address a trusted proxy reports in `Forwarded` / `X-Forwarded-For`. The client
//! state lives in a bounded cache ([`quick_cache`], sharded, frequency-aware eviction):
//! a flood of new keys evicts other rarely seen keys, never clients with requests in
//! flight, and memory stays at about `max_keys` × 100 bytes. Idle buckets (fully
//! refilled, nothing in flight) carry no information and are dropped by [`RateLimiter::sweep`].

use crate::obs::{Outcome, RequestReport};
use arc_swap::ArcSwap;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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
}

impl Class {
    pub const ALL: [Class; 4] = [Class::Auth, Class::Query, Class::Update, Class::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Class::Auth => "auth",
            Class::Query => "query",
            Class::Update => "update",
            Class::Admin => "admin",
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
        // reads that run queries over a dataset
        if admin.starts_with("schema/")
            || admin.starts_with("stats/")
            || admin == "reason/{ds}/diagnostics"
        {
            return Some(Class::Query);
        }
        return (!read && *method != Method::OPTIONS).then_some(Class::Admin);
    }
    match r {
        "/{ds}/sparql" | "/{ds}/query" | "/{ds}/get" | "/{ds}/explain" | "/{ds}/shacl" => {
            Some(Class::Query)
        }
        "/{ds}/update" | "/{ds}/upload" => Some(Class::Update),
        "/{ds}/data" => Some(if read { Class::Query } else { Class::Update }),
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

impl Limit {
    fn is_unlimited(&self) -> bool {
        self.rate.is_none() && self.concurrency.is_none() && self.client_concurrency.is_none()
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
    /// per class: `auth`, `query`, `update`, `admin`
    #[serde(default)]
    pub classes: BTreeMap<String, Limit>,
    /// per dataset and class; replaces the class limit on that dataset
    #[serde(default)]
    pub datasets: BTreeMap<String, BTreeMap<String, Limit>>,
    /// proxies whose `Forwarded` / `X-Forwarded-For` name the client (CIDR or address)
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// clients tracked at most (default 100000)
    pub max_keys: Option<usize>,
}

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
        if Class::parse(class).is_none() {
            return Err(format!(
                "--rate-limit '{spec}': unknown class '{class}' (auth, query, update or admin)"
            ));
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
        let check = |m: &BTreeMap<String, Limit>| -> anyhow::Result<()> {
            for (c, l) in m {
                if Class::parse(c).is_none() {
                    anyhow::bail!("unknown rate-limit class '{c}' (auth, query, update or admin)");
                }
                if l.burst.is_some() && l.rate.is_none() {
                    anyhow::bail!("{c}: burst needs a rate");
                }
            }
            Ok(())
        };
        check(&self.classes)?;
        for m in self.datasets.values() {
            check(m)?;
        }
        TrustedProxies::parse(&self.trusted_proxies).map_err(anyhow::Error::msg)?;
        Ok(())
    }

    /// Whether any limit is configured.
    pub fn is_empty(&self) -> bool {
        self.classes.values().all(Limit::is_unlimited)
            && self
                .datasets
                .values()
                .all(|m| m.values().all(Limit::is_unlimited))
    }
}

// -------------------------------------------------------------- client keys ------

/// Networks whose forwarding headers are believed.
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl TrustedProxies {
    /// Parse `10.0.0.0/8`, `::1`, `fd00::/8`, … .
    pub fn parse(items: &[String]) -> Result<TrustedProxies, String> {
        let mut v = Vec::new();
        for s in items {
            let (addr, len) = match s.split_once('/') {
                Some((a, l)) => (a, Some(l)),
                None => (s.as_str(), None),
            };
            let ip: IpAddr = addr
                .trim()
                .parse()
                .map_err(|_| format!("trusted proxy '{s}': not an IP address or CIDR"))?;
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
            v.push((ip, len));
        }
        Ok(TrustedProxies(v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.0.iter().any(|&(net, len)| match (net, ip) {
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

    /// The client address of a request that arrived from `peer`: when `peer` is trusted,
    /// the rightmost untrusted hop of `Forwarded` (RFC 7239), else of `X-Forwarded-For`.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if self.is_empty() || !self.contains(peer) {
            return peer;
        }
        let hops = forwarded_hops(headers);
        let mut client = peer;
        for hop in hops.iter().rev() {
            match hop {
                Some(ip) => {
                    client = *ip;
                    if !self.contains(*ip) {
                        break;
                    }
                }
                // an unparsable or obfuscated hop: stop at the last known address
                None => break,
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

/// The hops of `Forwarded` (`for=` of each element), else of `X-Forwarded-For`, left
/// (origin) to right (nearest proxy); `None` for a hop that is not an address.
fn forwarded_hops(headers: &HeaderMap) -> Vec<Option<IpAddr>> {
    let parse = |s: &str| -> Option<IpAddr> {
        let s = s.trim().trim_matches('"');
        if let Ok(ip) = s.parse() {
            return Some(ip);
        }
        // [v6]:port, v4:port
        if let Some(rest) = s.strip_prefix('[') {
            return rest.split(']').next()?.parse().ok();
        }
        s.parse::<SocketAddr>().ok().map(|a| a.ip())
    };
    let fwd: Vec<&str> = headers
        .get_all(header::FORWARDED)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if !fwd.is_empty() {
        return fwd
            .iter()
            .flat_map(|v| v.split(','))
            .map(|elem| {
                elem.split(';').find_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    k.trim().eq_ignore_ascii_case("for").then_some(v)
                })
            })
            .map(|v| v.and_then(parse))
            .collect();
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(parse)
        .collect()
}

/// Whom a request is counted against.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// an address (IPv4, or the /64 of an IPv6 address)
    Ip(u128),
    /// an authenticated principal
    #[cfg_attr(not(test), allow(dead_code))]
    Principal(Arc<str>),
    /// no address known (in-process requests): one shared key
    Unknown,
}

impl ClientKey {
    pub fn ip(ip: IpAddr) -> ClientKey {
        match canonical(ip) {
            IpAddr::V4(v4) => ClientKey::Ip(u128::from(v4.to_ipv6_mapped())),
            IpAddr::V6(v6) => ClientKey::Ip(u128::from(v6) & !u128::from(u64::MAX)),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
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
        match peer_ip(req) {
            Some(peer) => ClientKey::ip(trusted.client_ip(peer, req.headers())),
            None => ClientKey::Unknown,
        }
    }
}

/// The address the connection came from, when the server recorded it.
pub fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
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

/// One policy: a class, optionally on one dataset.
struct Policy {
    class: Class,
    /// `query` or `query@ds`, for the headers
    name: String,
    /// nanoseconds between requests at the sustained rate, and the burst
    gcra: Option<(u64, u32)>,
    rate: Option<Rate>,
    concurrency: Option<u32>,
    client_concurrency: Option<u32>,
    failure_cost: u32,
}

impl Policy {
    fn new(class: Class, dataset: Option<&str>, l: &Limit) -> Policy {
        let gcra = l.rate.map(|r| {
            let t = u64::try_from(r.period.as_nanos()).unwrap_or(u64::MAX) / u64::from(r.count);
            (t.max(1), l.burst.unwrap_or(r.count))
        });
        Policy {
            class,
            name: match dataset {
                Some(d) => format!("{}@{d}", class.as_str()),
                None => class.as_str().to_string(),
            },
            gcra,
            rate: l.rate,
            concurrency: l.concurrency,
            client_concurrency: l.client_concurrency,
            failure_cost: l.failure_cost.unwrap_or(1),
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

/// The state after a request was admitted: tokens left and seconds until full.
struct Admitted {
    remaining: u64,
    reset_secs: u64,
}

impl Slot {
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

    /// Charge without a check (failed-auth weighting).
    fn charge(&self, now: u64, t: u64, cost: u32) {
        let inc = t.saturating_mul(u64::from(cost));
        let _ = self
            .tat
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.max(now).saturating_add(inc))
            });
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

/// Clients with requests in flight are never evicted.
#[derive(Clone, Default)]
struct PinBusy;

impl quick_cache::Lifecycle<SlotKey, Arc<Slot>> for PinBusy {
    type RequestState = ();
    fn is_pinned(&self, _: &SlotKey, v: &Arc<Slot>) -> bool {
        v.inflight.load(Ordering::Relaxed) > 0
    }
}

type Slots = quick_cache::sync::Cache<
    SlotKey,
    Arc<Slot>,
    quick_cache::UnitWeighter,
    quick_cache::DefaultHashBuilder,
    PinBusy,
>;

/// One configuration's compiled policies and state (replaced wholesale on reload).
struct Inner {
    policies: Vec<Policy>,
    by_class: [Option<u32>; 4],
    by_dataset: BTreeMap<String, [Option<u32>; 4]>,
    /// requests in flight per policy, server-wide
    inflight: Vec<AtomicU32>,
    trusted: TrustedProxies,
    slots: Slots,
}

pub const DEFAULT_MAX_KEYS: usize = 100_000;

impl Inner {
    fn new(cfg: &Config) -> Result<Inner, String> {
        let mut policies = Vec::new();
        let mut add = |class: Class, ds: Option<&str>, l: &Limit| -> Option<u32> {
            if l.is_unlimited() {
                return None;
            }
            policies.push(Policy::new(class, ds, l));
            Some((policies.len() - 1) as u32)
        };
        let mut by_class = [None; 4];
        for (c, l) in &cfg.classes {
            let class = Class::parse(c).ok_or_else(|| format!("unknown class '{c}'"))?;
            by_class[class.index()] = add(class, None, l);
        }
        let mut by_dataset = BTreeMap::new();
        for (ds, m) in &cfg.datasets {
            // a dataset override for one class leaves the others at the class limit
            let mut per = by_class;
            for (c, l) in m {
                let class = Class::parse(c).ok_or_else(|| format!("unknown class '{c}'"))?;
                per[class.index()] = add(class, Some(ds), l);
            }
            by_dataset.insert(ds.clone(), per);
        }
        let max_keys = cfg.max_keys.unwrap_or(DEFAULT_MAX_KEYS).max(16);
        let slots = Slots::with_options(
            quick_cache::OptionsBuilder::new()
                .estimated_items_capacity(max_keys)
                .weight_capacity(max_keys as u64)
                .build()
                .map_err(|e| format!("{e:?}"))?,
            quick_cache::UnitWeighter,
            Default::default(),
            PinBusy,
        );
        Ok(Inner {
            inflight: policies.iter().map(|_| AtomicU32::new(0)).collect(),
            policies,
            by_class,
            by_dataset,
            trusted: TrustedProxies::parse(&cfg.trusted_proxies)?,
            slots,
        })
    }

    fn policy(&self, class: Class, dataset: Option<&str>) -> Option<u32> {
        dataset
            .and_then(|d| self.by_dataset.get(d))
            .map_or(self.by_class[class.index()], |p| p[class.index()])
    }

    fn slot(&self, policy: u32, client: ClientKey) -> Arc<Slot> {
        let key = SlotKey { policy, client };
        match self
            .slots
            .get_or_insert_with(&key, || Ok::<_, ()>(Arc::new(Slot::default())))
        {
            Ok(s) => s,
            Err(()) => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------- the limiter ------

/// Rate and concurrency limits of a server; shared by every request.
pub struct RateLimiter {
    inner: ArcSwap<Inner>,
    clock: Clock,
    keyer: Arc<dyn ClientKeyer>,
}

impl RateLimiter {
    pub fn new(cfg: &Config) -> Result<RateLimiter, String> {
        Ok(RateLimiter {
            inner: ArcSwap::from_pointee(Inner::new(cfg)?),
            clock: Clock::System(Instant::now()),
            keyer: Arc::new(PeerKeyer),
        })
    }

    /// Replace the clock (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_clock(mut self, clock: Clock) -> RateLimiter {
        self.clock = clock;
        self
    }

    /// Replace how clients are keyed (e.g. by authenticated principal).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_keyer(mut self, keyer: Arc<dyn ClientKeyer>) -> RateLimiter {
        self.keyer = keyer;
        self
    }

    /// Switch to a new configuration. Client state starts afresh; requests in flight
    /// finish under (and release) the old one.
    pub fn reload(&self, cfg: &Config) -> Result<(), String> {
        self.inner.store(Arc::new(Inner::new(cfg)?));
        Ok(())
    }

    /// Drop the state of idle clients (bucket full again, nothing in flight); returns
    /// how many were dropped.
    pub fn sweep(&self) -> usize {
        let inner = self.inner.load();
        let now = self.clock.now();
        let before = inner.slots.len();
        inner.slots.retain(|_, s| !s.idle(now));
        before.saturating_sub(inner.slots.len())
    }

    /// Clients currently tracked (over all policies).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn tracked(&self) -> usize {
        self.inner.load().slots.len()
    }
}

/// Where a server's rate-limit configuration comes from: `--rate-limit-config`, then
/// `--rate-limit` and `--rate-limit-trusted-proxy` on top.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub file: Option<std::path::PathBuf>,
    pub flags: Vec<String>,
    pub trusted_proxies: Vec<String>,
}

impl Sources {
    /// The merged configuration; `None` when nothing is configured and there is no
    /// file (no limiter at all, so no overhead).
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
        cfg.validate()?;
        Ok((self.file.is_some() || !cfg.is_empty()).then_some(cfg))
    }
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
/// when the response body is done.
struct Permits {
    server: Option<(Arc<Inner>, u32)>,
    client: Option<Arc<Slot>>,
}

impl Drop for Permits {
    fn drop(&mut self) {
        if let Some((inner, p)) = &self.server {
            inner.inflight[*p as usize].fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(s) = &self.client {
            s.inflight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// A response body that holds the request's permits until it is sent (streamed Graph
/// Store GETs keep working after the handler returned).
struct PermitBody {
    inner: Body,
    _permits: Permits,
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
    if let Ok(v) = HeaderValue::from_str(&policy) {
        h.insert("ratelimit-policy", v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{}\";r={remaining};t={reset}", p.name)) {
        h.insert("ratelimit", v);
    }
}

fn reject(p: &Policy, status: StatusCode, retry: u64, reason: &str, remaining: u64) -> Response {
    let class = p.class.as_str();
    let msg = if status == StatusCode::TOO_MANY_REQUESTS {
        format!("too many {class} requests: retry in {retry} s")
    } else {
        format!("too many concurrent {class} requests: retry in {retry} s")
    };
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

/// The middleware (`from_fn_with_state`), inside `obs::observe` so limited requests
/// are logged and counted.
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
    let slot = p
        .needs_slot()
        .then(|| inner.slot(pi, rl.keyer.key(class, &req, &inner.trusted)));
    let mut permits = Permits {
        server: None,
        client: None,
    };
    // concurrency first: it is released on rejection, a rate token is not
    if let Some(max) = p.concurrency {
        let n = inner.inflight[pi as usize].fetch_add(1, Ordering::Relaxed);
        permits.server = Some((inner.clone(), pi));
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
    let mut resp = next.run(req).await;
    if let Some(a) = admitted {
        let (mut remaining, mut reset) = (a.remaining, a.reset_secs);
        if p.failure_cost > 1
            && matches!(
                resp.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            )
            && let (Some((t, burst)), Some(s)) = (p.gcra, &slot)
        {
            let now = rl.clock.now();
            s.charge(now, t, p.failure_cost - 1);
            let debt = s.tat.load(Ordering::Relaxed).saturating_sub(now);
            remaining = (t * u64::from(burst)).saturating_sub(debt) / t;
            reset = debt.div_ceil(1_000_000_000);
        }
        rate_headers(resp.headers_mut(), p, remaining, reset);
    }
    if permits.server.is_some() || permits.client.is_some() {
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
