//! Outbound HTTP from the engine: SPARQL `SERVICE` and `LOAD <http…>`.
//!
//! Every request goes through an [`OutboundPolicy`]:
//!
//! - only `http` and `https` URLs;
//! - the host is resolved once, every address it resolves to must be allowed, and the
//!   connection goes to exactly those addresses (no second lookup that DNS rebinding
//!   could answer differently);
//! - each redirect hop is checked the same way, up to a number of hops;
//! - a connect timeout and a total timeout, and a ceiling on the response body enforced
//!   while it streams in;
//! - a budget over all the requests of one SPARQL request ([`RequestBudget`]): the bytes
//!   they receive and the time they take, summed.
//!
//! By default only public unicast addresses may be contacted: loopback, private
//! (RFC 1918), shared (100.64.0.0/10), link-local (among them the 169.254.169.254
//! metadata service), unique-local, multicast and the other special-purpose ranges are
//! refused, also in their IPv4-mapped, IPv4-compatible, NAT64 and 6to4 IPv6 forms.
//! [`OutboundPolicy::allow_private`] opens the private ranges, and an address or network
//! in [`OutboundPolicy::allow`] opens the addresses it covers; a host name in the
//! allowlist does not open non-public addresses.
//!
//! Error messages name what the caller sent (the URL, its host) but never an address a
//! name resolved to or the underlying connection error: those are logged (target
//! `sparkles::outbound`) for the operator.
//!
//! An embedder can also install one process-wide hook adding headers to every request,
//! for example to propagate W3C Trace Context (`traceparent`) from the current
//! `tracing` span; the engine itself depends on no telemetry SDK. The hook runs on the
//! thread making the request, inside the request's `tracing` span.

use ipnet::IpNet;
use parking_lot::Mutex;
use reqwest::Url;
use std::fmt;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Refuse loopback, private and other non-public destinations unless the policy allows
/// them: the default of [`OutboundPolicy::allow_private`] is its negation.
pub const BLOCK_PRIVATE_BY_DEFAULT: bool = true;
/// Default time to establish a connection (DNS, TCP and TLS).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default total time of a request, until the end of the response body.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default ceiling on a response body.
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 256 << 20;
/// Default number of redirects followed.
pub const DEFAULT_MAX_REDIRECTS: usize = 5;
/// Default bytes all the outbound requests of one SPARQL request may receive: four
/// full-size responses.
pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 4 * DEFAULT_MAX_RESPONSE_BYTES;
/// Inputs per request of a bulk SERVICE without a size, as in Jena.
pub const DEFAULT_SERVICE_BULK_SIZE: usize = 10;
/// The most inputs per request of a bulk SERVICE, as in Jena.
pub const DEFAULT_SERVICE_BULK_MAX: usize = 100;
/// Default time all the outbound requests of one SPARQL request may take, summed.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(4 * 60);

/// Resolves host names for outbound requests.
pub trait Resolver: Send + Sync {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>>;
}

/// The system resolver (`getaddrinfo`).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        Ok((host, 0).to_socket_addrs()?.map(|a| a.ip()).collect())
    }
}

/// An allowlist entry, parsed from `host.example.org`, `*.example.org`, an address or
/// a CIDR network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Allow {
    /// This host name, at public addresses (private ones too with
    /// [`OutboundPolicy::allow_private`]); a name resolving to a non-public address
    /// also needs a [`Allow::Net`] entry covering that address, so a hijacked or
    /// misconfigured name never reaches the metadata service.
    Host(String),
    /// Subdomains of this name (`*.example.org`, not `example.org` itself); their
    /// addresses must still be public unless [`OutboundPolicy::allow_private`].
    Subdomains(String),
    /// Addresses in this network, private ones included.
    Net(IpNet),
}

impl FromStr for Allow {
    type Err = String;

    fn from_str(s: &str) -> Result<Allow, String> {
        let s = s.trim();
        if let Ok(n) = s.parse::<IpNet>() {
            return Ok(Allow::Net(n.trunc()));
        }
        if let Some(ip) = ip_of_host(s) {
            return Ok(Allow::Net(IpNet::from(ip)));
        }
        let (wild, name) = match s.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let bad = || format!("{s:?} is not a host name, address or CIDR network");
        let host = normalize_host(name).ok_or_else(bad)?;
        match ip_of_host(&host) {
            Some(_) if wild => Err(bad()),
            Some(ip) => Ok(Allow::Net(IpNet::from(ip))),
            None if wild => Ok(Allow::Subdomains(host)),
            None => Ok(Allow::Host(host)),
        }
    }
}

impl fmt::Display for Allow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Allow::Host(h) => f.write_str(h),
            Allow::Subdomains(h) => write!(f, "*.{h}"),
            Allow::Net(n) => write!(f, "{n}"),
        }
    }
}

/// The host of `http://{name}/` as a URL parser sees it (lowercase, IDNA, IPv4 in
/// dotted form), without a trailing dot; `None` if `name` is more than a host.
fn normalize_host(name: &str) -> Option<String> {
    if name.contains([':', '/', '\\', '@', '?', '#']) {
        return None;
    }
    let u = Url::parse(&format!("http://{name}/")).ok()?;
    let h = u.host_str()?.trim_end_matches('.');
    (!h.is_empty()).then(|| h.to_string())
}

/// The address of an IP-literal URL host (`127.0.0.1`, `[::1]`).
fn ip_of_host(host: &str) -> Option<IpAddr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse().ok()
}

/// What kind of destination an address is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrClass {
    /// A public unicast address.
    Public,
    /// Loopback, private and the other non-public unicast ranges: contacted with
    /// [`OutboundPolicy::allow_private`] or an allowlist network that covers them.
    Private(&'static str),
    /// Link-local (the cloud metadata service), multicast, broadcast, unspecified and
    /// reserved addresses: contacted only with an allowlist network that covers them.
    Restricted(&'static str),
}

/// Classify an address; IPv6 forms that embed an IPv4 address (mapped, compatible,
/// NAT64, 6to4) are classified by that address.
pub fn classify(ip: IpAddr) -> AddrClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => classify_v4(v4),
            None => classify_v6(v6),
        },
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddrClass {
    use AddrClass::*;
    match ip.octets() {
        [0, ..] => Restricted("\"this network\""),
        [127, ..] => Private("loopback"),
        [10, ..] | [172, 16..=31, ..] | [192, 168, ..] => Private("private"),
        [100, 64..=127, ..] => Private("shared (carrier-grade NAT)"),
        [169, 254, 169, 254] => Restricted("link-local (cloud metadata)"),
        [169, 254, ..] => Restricted("link-local"),
        [192, 0, 0, _] => Private("IETF protocol assignment"),
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _] => Private("documentation"),
        [192, 88, 99, _] => Private("6to4 relay"),
        [198, 18..=19, ..] => Private("benchmarking"),
        [224..=239, ..] => Restricted("multicast"),
        [255, 255, 255, 255] => Restricted("broadcast"),
        [240..=255, ..] => Restricted("reserved"),
        _ => Public,
    }
}

fn classify_v6(ip: Ipv6Addr) -> AddrClass {
    use AddrClass::*;
    let s = ip.segments();
    if ip.is_unspecified() {
        return Restricted("unspecified");
    }
    if ip.is_loopback() {
        return Private("loopback");
    }
    match s[0] {
        0x0064 if s[1] == 0xff9b && s[2] == 1 => Private("NAT64 local-use"),
        0x0100 if s[1..4] == [0, 0, 0] => Restricted("discard-only"),
        0x2001 if s[1] == 0 => Private("Teredo"),
        0x2001 if s[1] == 0x0db8 => Private("documentation"),
        0x2001 if s[1] == 2 && s[2] == 0 => Private("benchmarking"),
        0x2001 if s[1] < 0x0200 => Private("IETF protocol assignment"),
        0x3fff if s[1] < 0x1000 => Private("documentation"),
        x if x & 0xfe00 == 0xfc00 => Private("unique local"),
        x if x & 0xffc0 == 0xfe80 => Restricted("link-local"),
        x if x & 0xffc0 == 0xfec0 => Private("site-local"),
        x if x & 0xff00 == 0xff00 => Restricted("multicast"),
        x if x & 0xe000 == 0x2000 => Public,
        _ => Restricted("reserved"),
    }
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`), IPv4-translated
/// (`::ffff:0:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`) or
/// 6to4 (`2002:aabb:ccdd::`) address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let low = |a: u16, b: u16| Ipv4Addr::from((u32::from(a) << 16) | u32::from(b));
    match s {
        [0, 0, 0, 0, 0, 0xffff, a, b] | [0, 0, 0, 0, 0xffff, 0, a, b] => Some(low(a, b)),
        // `::` and `::1` are not IPv4-compatible
        [0, 0, 0, 0, 0, 0, a, b] if a != 0 || b > 1 => Some(low(a, b)),
        [0x0064, 0xff9b, 0, 0, 0, 0, a, b] => Some(low(a, b)),
        [0x2002, a, b, ..] => Some(low(a, b)),
        _ => None,
    }
}

/// The network policy of outbound requests (see the [module docs](self)).
#[derive(Clone)]
pub struct OutboundPolicy {
    /// Contact [`AddrClass::Private`] addresses (loopback, RFC 1918, …).
    pub allow_private: bool,
    /// When non-empty, only these destinations are contacted. An [`Allow::Net`] entry
    /// also admits the non-public addresses it covers; name entries do not.
    pub allow: Vec<Allow>,
    /// Time to establish a connection.
    pub connect_timeout: Duration,
    /// Total time of a request, until the end of the response body.
    pub timeout: Duration,
    /// Ceiling on the response body, in bytes (decompressed, for a compressed `LOAD`).
    pub max_response_bytes: u64,
    /// Redirects followed; every hop is checked like the first URL.
    pub max_redirects: usize,
    /// Bytes all the requests of one SPARQL request may receive (see
    /// [`RequestBudget`]); past it the request fails with
    /// [`BudgetKind::OutboundBytes`](crate::error::BudgetKind::OutboundBytes).
    pub max_request_bytes: u64,
    /// Time all the requests of one SPARQL request may take, summed.
    pub request_timeout: Duration,
    /// Inputs per request of a `SERVICE <loop:bulk:…>` (Jena's
    /// `serviceBulkBindingCount`).
    pub service_bulk_size: usize,
    /// The most inputs per request of any bulk SERVICE; `bulk+n` is capped to it (Jena's
    /// `serviceBulkMaxBindingCount`).
    pub service_bulk_max: usize,
    pub resolver: Arc<dyn Resolver>,
    /// How the requests check the certificates of `https` servers. The default trusts
    /// the system's roots and verifies every certificate.
    pub tls: TlsOptions,
}

/// The certificate checks of a policy's `https` requests, such as those of one model
/// provider (spec C19 §11.6). They change only how a server's certificate is verified:
/// every destination is still checked against the policy, and a redirect to another
/// origin is refused while either option is set, so they never apply past the server
/// they were given for.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// PEM certificates (one or a bundle) trusted in addition to the system's roots.
    pub extra_roots_pem: Option<Vec<u8>>,
    /// Accept any certificate. Whoever is on the network path can then read and change
    /// the requests and their answers.
    pub insecure_skip_verify: bool,
}

impl TlsOptions {
    /// Whether either option is set.
    pub fn is_custom(&self) -> bool {
        self.extra_roots_pem.is_some() || self.insecure_skip_verify
    }
}

impl fmt::Debug for TlsOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsOptions")
            .field("extra_roots", &self.extra_roots_pem.is_some())
            .field("insecure_skip_verify", &self.insecure_skip_verify)
            .finish()
    }
}

/// The number of certificates in a PEM text, or why it holds none that can be used.
pub fn check_pem_bundle(pem: &[u8]) -> Result<usize, String> {
    match reqwest::Certificate::from_pem_bundle(pem) {
        Ok(c) if c.is_empty() => Err("it holds no PEM certificate".into()),
        Ok(c) => Ok(c.len()),
        Err(_) => Err("it is not a PEM certificate or bundle".into()),
    }
}

impl Default for OutboundPolicy {
    fn default() -> OutboundPolicy {
        OutboundPolicy {
            allow_private: !BLOCK_PRIVATE_BY_DEFAULT,
            allow: Vec::new(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            service_bulk_size: DEFAULT_SERVICE_BULK_SIZE,
            service_bulk_max: DEFAULT_SERVICE_BULK_MAX,
            resolver: Arc::new(SystemResolver),
            tls: TlsOptions::default(),
        }
    }
}

impl fmt::Debug for OutboundPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutboundPolicy")
            .field("allow_private", &self.allow_private)
            .field("allow", &self.allow)
            .field("connect_timeout", &self.connect_timeout)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_redirects", &self.max_redirects)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("request_timeout", &self.request_timeout)
            .field("service_bulk_size", &self.service_bulk_size)
            .field("service_bulk_max", &self.service_bulk_max)
            .field("tls", &self.tls)
            .finish_non_exhaustive()
    }
}

/// Why a request did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The policy refused the destination (before any connection to it).
    Refused(String),
    /// The request failed: bad URL, network error, timeout, oversized response.
    Failed(String),
    /// The SPARQL request's outbound byte budget is spent.
    Budget(crate::error::Budget),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(m) | Failure::Failed(m) => f.write_str(m),
            Failure::Budget(b) => write!(f, "{b}"),
        }
    }
}

impl Failure {
    /// The engine error of a failed `what` (`SERVICE <url>`, `LOAD <url>`): a refusal is
    /// [`Error::NotPermitted`](crate::Error::NotPermitted), a spent budget
    /// [`Error::BudgetExceeded`](crate::Error::BudgetExceeded), and a failure what
    /// `failed` makes of its message.
    pub(crate) fn into_error(
        self,
        what: &str,
        failed: impl FnOnce(String) -> crate::Error,
    ) -> crate::Error {
        match self {
            Failure::Refused(m) => crate::Error::NotPermitted(format!("{what}: {m}")),
            Failure::Failed(m) => failed(m),
            Failure::Budget(b) => crate::Error::BudgetExceeded(b),
        }
    }
}

/// What the outbound requests of one SPARQL request (a query, or an update with all its
/// operations) have used of the policy's [`max_request_bytes`] and [`request_timeout`]:
/// every SERVICE call and `LOAD` of the request draws on the same budget, so many small
/// requests cannot add up to more than one large one may.
///
/// Bytes are counted as responses stream in; time from the start of each request to the
/// end of its body (calls running at once each count their own time).
///
/// [`max_request_bytes`]: OutboundPolicy::max_request_bytes
/// [`request_timeout`]: OutboundPolicy::request_timeout
#[derive(Debug)]
pub struct RequestBudget {
    max_bytes: u64,
    max_time: Duration,
    bytes: AtomicU64,
    nanos: AtomicU64,
}

impl RequestBudget {
    /// A fresh budget under `policy`'s totals.
    pub fn new(policy: &OutboundPolicy) -> Arc<RequestBudget> {
        Arc::new(RequestBudget {
            max_bytes: policy.max_request_bytes,
            max_time: policy.request_timeout,
            bytes: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
        })
    }

    /// Bytes received so far.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Time the requests took so far, summed.
    pub fn time(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }

    fn remaining_time(&self) -> Duration {
        self.max_time.saturating_sub(self.time())
    }

    fn remaining_bytes(&self) -> u64 {
        self.max_bytes.saturating_sub(self.bytes())
    }

    fn spend_time(&self, d: Duration) {
        let n = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let _ = self
            .nanos
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| {
                Some(t.saturating_add(n))
            });
    }

    /// Count `n` more bytes; the exceeded budget once past the total.
    fn spend_bytes(&self, n: u64) -> Result<(), crate::error::Budget> {
        let used = self.bytes.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        if used > self.max_bytes {
            return Err(self.exceeded(used));
        }
        Ok(())
    }

    fn exceeded(&self, requested: u64) -> crate::error::Budget {
        crate::error::Budget {
            kind: crate::error::BudgetKind::OutboundBytes,
            limit: self.max_bytes,
            requested,
        }
    }
}

impl OutboundPolicy {
    /// Parse and check a URL: its scheme and, for an IP-literal host, the address.
    /// A host name is checked when it is resolved ([`check_host`](Self::check_host)).
    pub fn check_url(&self, url: &str) -> Result<Url, Failure> {
        let u = Url::parse(url).map_err(|e| Failure::Failed(format!("invalid URL: {e}")))?;
        self.check_parsed(&u)?;
        Ok(u)
    }

    fn check_parsed(&self, u: &Url) -> Result<(), Failure> {
        if !matches!(u.scheme(), "http" | "https") {
            return Err(Failure::Failed(format!(
                "unsupported scheme '{}' (only http and https)",
                u.scheme()
            )));
        }
        let host = u
            .host_str()
            .ok_or_else(|| Failure::Failed("the URL has no host".into()))?;
        match ip_of_host(host) {
            Some(ip) => self.check_addrs(None, &[ip]),
            // without networks in the allowlist, a name it lacks is refused before DNS
            None if !self.allow.is_empty()
                && !self.allow.iter().any(|a| matches!(a, Allow::Net(_)))
                && !self.names_allow(host) =>
            {
                Err(not_allowed(host))
            }
            None => Ok(()),
        }
    }

    /// Resolve a host name and check every address; the addresses to connect to.
    pub fn check_host(&self, host: &str) -> Result<Vec<IpAddr>, Failure> {
        let addrs = self.resolver.resolve(host).map_err(|e| {
            tracing::info!(target: "sparkles::outbound", host, error = %e, "cannot resolve");
            Failure::Failed(format!("cannot resolve {host}"))
        })?;
        if addrs.is_empty() {
            return Err(Failure::Failed(format!("cannot resolve {host}")));
        }
        self.check_addrs(Some(host), &addrs)?;
        Ok(addrs)
    }

    /// Whether an allowlist name entry matches the host.
    fn names_allow(&self, host: &str) -> bool {
        let h = host.trim_end_matches('.').to_ascii_lowercase();
        self.allow.iter().any(|a| match a {
            Allow::Host(n) => *n == h,
            Allow::Subdomains(n) => h
                .strip_suffix(n.as_str())
                .is_some_and(|p| p.len() > 1 && p.ends_with('.')),
            Allow::Net(_) => false,
        })
    }

    /// Check the addresses of a destination (`host`: its name, `None` for an IP
    /// literal). Any refused address refuses the destination.
    ///
    /// The refusal of a name does not say what it resolved to (an internal name's
    /// address is not the caller's business); the address and its kind are logged.
    fn check_addrs(&self, host: Option<&str>, addrs: &[IpAddr]) -> Result<(), Failure> {
        let h = host.map(|h| h.trim_end_matches('.').to_ascii_lowercase());
        let by_name = h.as_deref().is_some_and(|h| self.names_allow(h));
        for &ip in addrs {
            let v4 = match ip {
                IpAddr::V6(v6) => embedded_v4(v6).map(IpAddr::V4),
                IpAddr::V4(_) => None,
            };
            let by_net = self.allow.iter().any(|a| match a {
                Allow::Net(n) => n.contains(&ip) || v4.is_some_and(|v| n.contains(&v)),
                _ => false,
            });
            if by_net {
                continue;
            }
            match classify(ip) {
                AddrClass::Public => {}
                AddrClass::Private(_) if self.allow_private => {}
                AddrClass::Private(what) | AddrClass::Restricted(what) => {
                    return Err(Failure::Refused(match &h {
                        Some(h) => {
                            tracing::warn!(
                                target: "sparkles::outbound",
                                host = %h,
                                address = %ip,
                                "refused: {h} resolves to {ip}, a {what} address"
                            );
                            format!("{h} is refused by the outbound policy")
                        }
                        None => format!("{ip} is a {what} address"),
                    }));
                }
            }
            if !self.allow.is_empty() && !by_name {
                return Err(not_allowed(h.as_deref().unwrap_or(&ip.to_string())));
            }
        }
        Ok(())
    }

    /// Send the request `build` makes for `url`, within `timeout` (at most the policy's,
    /// and at most what is left of `budget`), and return the response once its headers
    /// are in. The response's time and bytes are counted in `budget`.
    pub(crate) fn send(
        &self,
        budget: &Arc<RequestBudget>,
        url: &str,
        timeout: Duration,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, Failure> {
        let u = self.check_url(url)?;
        let own = timeout.min(self.timeout);
        let left = budget.remaining_time();
        if left.is_zero() {
            return Err(Failure::Failed(out_of_time(budget.max_time)));
        }
        let timeout = own.min(left);
        // the message of a timeout: this request's, or the whole SPARQL request's
        let late = if left < own {
            out_of_time(budget.max_time)
        } else {
            timed_out(timeout)
        };
        let failed = Arc::new(Mutex::new(None));
        let client = self
            .client(&failed)
            // a client that cannot be built is a local configuration error (such as an
            // `SSL_CERT_FILE` that names no file), so its cause is safe to show
            .map_err(|e| {
                Failure::Failed(format!(
                    "{}: {}",
                    hidden(url, "the HTTP client could not be built", &e),
                    chain(&e)
                ))
            })?;
        let start = Instant::now();
        let resp = apply(build(&client, u)).timeout(timeout).send();
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                budget.spend_time(start.elapsed());
                // a refused or unresolvable name, in words fit for the caller
                if let Some(f) = failed.lock().take() {
                    return Err(f);
                }
                if e.is_timeout() || start.elapsed() >= timeout {
                    return Err(Failure::Failed(late));
                }
                // redirect errors are the policy's own words (hops, schemes)
                if e.is_redirect() {
                    return Err(Failure::Failed(chain(&e)));
                }
                let what = if e.is_connect() && chain(&e).contains("invalid peer certificate") {
                    "cannot connect: the server's TLS certificate is not trusted"
                } else if e.is_connect() {
                    "cannot connect"
                } else {
                    "the request failed"
                };
                return Err(Failure::Failed(hidden(url, what, &e)));
            }
        };
        let limit = self.max_response_bytes;
        let body = Body {
            resp,
            read: 0,
            limit,
            budget: Some(budget.clone()),
            spent: budget.clone(),
            start,
            deadline: start + timeout,
            late,
        };
        if let Some(n) = body.resp.content_length() {
            if n > limit {
                return Err(Failure::Failed(too_large(limit)));
            }
            if n > budget.remaining_bytes() {
                return Err(Failure::Budget(budget.exceeded(budget.bytes() + n)));
            }
        }
        let content_type = body
            .resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let status = body.resp.status();
        Ok(Response {
            status,
            content_type,
            body,
        })
    }

    /// A client that resolves (and so connects) only through the policy, checks every
    /// redirect, and records a refusal or a failed lookup in `failed`.
    fn client(
        &self,
        failed: &Arc<Mutex<Option<Failure>>>,
    ) -> reqwest::Result<reqwest::blocking::Client> {
        let (p, slot, max) = (self.clone(), failed.clone(), self.max_redirects);
        let custom = self.tls.is_custom();
        let redirect = reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() > max {
                return a.error(format!("more than {max} redirects"));
            }
            // the TLS options were given for the first server only
            if custom
                && a.previous()
                    .first()
                    .is_some_and(|first| first.origin() != a.url().origin())
            {
                let m = format!(
                    "redirect to {}: another server, while the TLS options are set for {}",
                    a.url(),
                    a.previous()[0].origin().ascii_serialization()
                );
                *slot.lock() = Some(Failure::Refused(m.clone()));
                return a.error(m);
            }
            match p.check_parsed(a.url()) {
                Ok(()) => a.follow(),
                Err(f) => {
                    let m = format!("redirect to {}: {f}", a.url());
                    if matches!(f, Failure::Refused(_)) {
                        *slot.lock() = Some(Failure::Refused(m.clone()));
                    }
                    a.error(m)
                }
            }
        });
        let mut b = reqwest::blocking::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .redirect(redirect)
            // a proxy would resolve and connect on its own, past the checks
            .no_proxy()
            .dns_resolver(Arc::new(Pinned {
                policy: self.clone(),
                failed: failed.clone(),
            }))
            .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")));
        if let Some(pem) = &self.tls.extra_roots_pem {
            b = b.tls_certs_merge(reqwest::Certificate::from_pem_bundle(pem)?);
        }
        if self.tls.insecure_skip_verify {
            b = b.tls_danger_accept_invalid_certs(true);
        }
        b.build()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The resolver of a request's client: resolves through the policy and hands the
/// checked addresses, and only those, to the connector.
struct Pinned {
    policy: OutboundPolicy,
    failed: Arc<Mutex<Option<Failure>>>,
}

impl reqwest::dns::Resolve for Pinned {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (policy, failed) = (self.policy.clone(), self.failed.clone());
        let host = name.as_str().to_string();
        Box::pin(async move {
            let checked = tokio::task::spawn_blocking(move || policy.check_host(&host)).await?;
            let r: Result<reqwest::dns::Addrs, BoxError> = match checked {
                Ok(addrs) => Ok(Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)))),
                Err(f) => {
                    let m = f.to_string();
                    *failed.lock() = Some(f);
                    Err(m.into())
                }
            };
            r
        })
    }
}

fn not_allowed(host: &str) -> Failure {
    Failure::Refused(format!("{host} is not in the outbound allowlist"))
}

fn timed_out(timeout: Duration) -> String {
    format!("no complete response within {}", secs(timeout))
}

fn out_of_time(total: Duration) -> String {
    format!(
        "the outbound requests (SERVICE, LOAD) of this request took longer than their total of {}",
        secs(total)
    )
}

/// A failure message that keeps the error's details (addresses, OS errors) out of what
/// the caller sees; they are logged.
fn hidden(url: &str, what: &str, e: &dyn std::error::Error) -> String {
    // without the credentials a URL may carry
    let url = Url::parse(url).map_or_else(
        |_| String::new(),
        |mut u| {
            let _ = u.set_password(None);
            let _ = u.set_username("");
            u.to_string()
        },
    );
    tracing::warn!(target: "sparkles::outbound", url, error = %chain(e), "{what}");
    what.to_string()
}

fn too_large(limit: u64) -> String {
    let size = if limit >= 1 << 20 && limit.is_multiple_of(1 << 20) {
        format!("{} MiB", limit >> 20)
    } else {
        format!("{limit} bytes")
    };
    format!("the response is larger than the outbound limit of {size}")
}

fn secs(d: Duration) -> String {
    format!("{} s", d.as_secs_f64())
}

/// An error and its sources, `: `-separated.
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(c) = src {
        let m = c.to_string();
        if !s.contains(&m) {
            s.push_str(": ");
            s.push_str(&m);
        }
        src = c.source();
    }
    s
}

/// GET `url` through `policy` and return its body as UTF-8 text (a ShEx `IMPORT`, for
/// example), with `accept` as the `Accept` header. The response is held to the policy's
/// timeout and [`max_response_bytes`](OutboundPolicy::max_response_bytes), and counted
/// in `budget`. A refused destination is [`Error::NotPermitted`](crate::Error), a spent
/// budget [`Error::BudgetExceeded`](crate::Error), anything else (an HTTP error status,
/// a network failure, a body that is not UTF-8) [`Error::Invalid`](crate::Error).
pub fn fetch_text(
    policy: &OutboundPolicy,
    budget: &Arc<RequestBudget>,
    url: &str,
    accept: &str,
) -> crate::Result<String> {
    let failed = |m: String| crate::Error::invalid(format!("GET {url}: {m}"));
    let (bytes, _) = fetch_bytes(policy, budget, url, accept)?;
    String::from_utf8(bytes).map_err(|_| failed("the response is not UTF-8".into()))
}

/// GET `url` through `policy` as [`fetch_text`] does, and return its body as bytes with
/// the response's `Content-Type`.
pub fn fetch_bytes(
    policy: &OutboundPolicy,
    budget: &Arc<RequestBudget>,
    url: &str,
    accept: &str,
) -> crate::Result<(Vec<u8>, String)> {
    let failed = |m: String| crate::Error::invalid(format!("GET {url}: {m}"));
    let resp = policy
        .send(budget, url, policy.timeout, |client, u| {
            client.get(u).header("Accept", accept)
        })
        .map_err(|f| f.into_error(&format!("GET <{url}>"), failed))?;
    if !resp.status.is_success() {
        return Err(failed(resp.status.to_string()));
    }
    let content_type = resp.content_type.clone();
    let mut bytes = Vec::new();
    let mut body = resp.body;
    body.read_to_end(&mut bytes)
        .map_err(|e| match crate::codec::io_error(e) {
            crate::Error::Io(e) => failed(e.to_string()),
            e => e,
        })?;
    Ok((bytes, content_type))
}

/// GET `url` through `policy` with `headers` and return the status with the body as a
/// stream, under the policy's response ceiling and `timeout` (at most the policy's). Any
/// status is an answer; a refused destination or a network failure is a [`Failure`].
/// Model downloads (spec F12) use it with a policy whose ceiling and timeout fit
/// files of several gigabytes.
pub fn get_stream(
    policy: &OutboundPolicy,
    url: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<(u16, Box<dyn Read + Send>), Failure> {
    let budget = RequestBudget::new(policy);
    let resp = policy.send(&budget, url, timeout, |client, u| {
        let mut rb = client.get(u);
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb
    })?;
    Ok((resp.status.as_u16(), Box::new(resp.body)))
}

/// The answer of [`post_json`]: the status, the `Retry-After` header and the body.
pub struct Posted {
    pub status: reqwest::StatusCode,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

/// POST `body` (JSON) to `url` through `policy`, with `headers`, within `timeout` (at
/// most the policy's), and read the whole response body under the policy's ceiling. Any
/// status is an answer; only a refused destination, a network failure, a timeout or an
/// oversized body is a [`Failure`].
pub fn post_json(
    policy: &OutboundPolicy,
    url: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Posted, Failure> {
    let budget = RequestBudget::new(policy);
    let resp = policy.send(&budget, url, timeout, |client, u| {
        let mut rb = client
            .post(u)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.body(body)
    })?;
    let retry_after = resp
        .body
        .resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let status = resp.status;
    let mut bytes = Vec::new();
    let mut b = resp.body;
    b.read_to_end(&mut bytes)
        .map_err(|e| Failure::Failed(e.to_string()))?;
    Ok(Posted {
        status,
        retry_after,
        body: bytes,
    })
}

/// A response whose headers are in.
pub(crate) struct Response {
    pub status: reqwest::StatusCode,
    pub content_type: String,
    pub body: Body,
}

/// A response body under the policy's byte ceiling and deadline, counted in the SPARQL
/// request's budget. Its read errors carry a message fit for the caller; a spent byte
/// budget is an [`Error::BudgetExceeded`](crate::Error::BudgetExceeded) inside the
/// `io::Error` (see [`crate::codec::io_error`]).
pub(crate) struct Body {
    resp: reqwest::blocking::Response,
    read: u64,
    limit: u64,
    /// counts the bytes read (`None`: the reader counts them itself)
    budget: Option<Arc<RequestBudget>>,
    /// counts the time until the body is dropped
    spent: Arc<RequestBudget>,
    start: Instant,
    deadline: Instant,
    /// the message of a timeout
    late: String,
}

impl Body {
    /// Stop counting the bytes read in the budget: the caller counts them after
    /// decompression instead (a compressed `LOAD`).
    pub fn uncounted(mut self) -> (Body, Arc<RequestBudget>) {
        self.budget = None;
        let b = self.spent.clone();
        (self, b)
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.spent.spend_time(self.start.elapsed());
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let late = |d: Instant| Instant::now() >= d;
        if late(self.deadline) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, self.late.clone()));
        }
        let n = match self.resp.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut || late(self.deadline) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, self.late.clone()));
            }
            Err(e) => {
                let url = self.resp.url().to_string();
                return Err(io::Error::new(
                    e.kind(),
                    hidden(&url, "the connection failed while reading the response", &e),
                ));
            }
        };
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other(too_large(self.limit)));
        }
        if let Some(b) = &self.budget {
            b.spend_bytes(n as u64)
                .map_err(|b| io::Error::other(crate::Error::BudgetExceeded(b)))?;
        }
        Ok(n)
    }
}

/// A reader whose bytes are counted in a SPARQL request's outbound budget.
pub(crate) struct Counted<R> {
    pub inner: R,
    pub budget: Arc<RequestBudget>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.budget
            .spend_bytes(n as u64)
            .map_err(|b| io::Error::other(crate::Error::BudgetExceeded(b)))?;
        Ok(n)
    }
}

type Hook = dyn Fn(&mut dyn FnMut(&str, &str)) + Send + Sync;

static HOOK: OnceLock<Box<Hook>> = OnceLock::new();

/// Install the hook: it is called once per outbound request and passes each header to
/// add to its argument. Returns `false` (and changes nothing) when a hook is already
/// installed.
pub fn set_headers_hook(hook: impl Fn(&mut dyn FnMut(&str, &str)) + Send + Sync + 'static) -> bool {
    HOOK.set(Box::new(hook)).is_ok()
}

/// Add the hook's headers to a request.
fn apply(rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    let Some(hook) = HOOK.get() else {
        return rb;
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    hook(&mut |k, v| headers.push((k.to_string(), v.to_string())));
    headers.into_iter().fold(rb, |rb, (k, v)| rb.header(k, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A resolver answering from a table.
    struct Fake(HashMap<&'static str, Vec<IpAddr>>);

    impl Resolver for Fake {
        fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
            self.0
                .get(host.trim_end_matches('.').to_ascii_lowercase().as_str())
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn policy(entries: &[(&'static str, &[&str])]) -> OutboundPolicy {
        OutboundPolicy {
            resolver: Arc::new(Fake(
                entries
                    .iter()
                    .map(|(h, a)| (*h, a.iter().map(|s| ip(s)).collect()))
                    .collect(),
            )),
            ..Default::default()
        }
    }

    fn refused<T: fmt::Debug>(r: Result<T, Failure>) -> String {
        match r {
            Err(Failure::Refused(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn classifies_special_purpose_addresses() {
        let public = [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ];
        for a in public {
            assert_eq!(classify(ip(a)), AddrClass::Public, "{a}");
        }
        let private = [
            ("127.0.0.1", "loopback"),
            ("127.255.0.9", "loopback"),
            ("10.1.2.3", "private"),
            ("172.16.0.1", "private"),
            ("172.31.255.255", "private"),
            ("192.168.1.1", "private"),
            ("100.64.0.1", "shared (carrier-grade NAT)"),
            ("100.127.255.254", "shared (carrier-grade NAT)"),
            ("192.0.2.1", "documentation"),
            ("198.51.100.7", "documentation"),
            ("203.0.113.9", "documentation"),
            ("198.18.0.1", "benchmarking"),
            ("::1", "loopback"),
            ("fc00::1", "unique local"),
            ("fd12:3456::1", "unique local"),
            ("fec0::1", "site-local"),
            ("2001:db8::1", "documentation"),
            ("2001:0:4136:e378::1", "Teredo"),
            ("64:ff9b:1::a00:1", "NAT64 local-use"),
            // IPv4 forms inside IPv6
            ("::ffff:127.0.0.1", "loopback"),
            ("::ffff:0:10.0.0.1", "private"),
            ("::127.0.0.1", "loopback"),
            ("64:ff9b::7f00:1", "loopback"),
            ("64:ff9b::a9fe:a9fe", "link-local (cloud metadata)"),
            ("2002:c0a8:101::1", "private"),
        ];
        for (a, what) in private {
            let c = classify(ip(a));
            assert!(
                c == AddrClass::Private(what) || c == AddrClass::Restricted(what),
                "{a}: {c:?}"
            );
        }
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "169.254.169.254",
            "169.254.1.1",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "100::1",
            "::ffff:169.254.169.254",
            "4000::1",
        ] {
            assert!(
                matches!(classify(ip(a)), AddrClass::Restricted(_)),
                "{a}: {:?}",
                classify(ip(a))
            );
        }
        // not IPv4-compatible, nor anything else special
        assert_eq!(classify(ip("172.32.0.1")), AddrClass::Public);
        assert_eq!(classify(ip("100.128.0.1")), AddrClass::Public);
    }

    #[test]
    fn ip_literal_urls() {
        let p = OutboundPolicy::default();
        for (url, msg) in [
            (
                "http://127.0.0.1:3030/ds",
                "127.0.0.1 is a loopback address",
            ),
            ("http://localhost.:1/", ""),
            ("http://2130706433/", "127.0.0.1 is a loopback address"),
            ("http://0x7f.1/", "127.0.0.1 is a loopback address"),
            ("http://10.0.0.8/", "10.0.0.8 is a private address"),
            ("http://192.168.0.1/", "192.168.0.1 is a private address"),
            ("http://100.64.1.1/", "100.64.1.1 is a shared"),
            (
                "http://169.254.169.254/latest/meta-data/",
                "169.254.169.254 is a link-local (cloud metadata) address",
            ),
            ("http://[::1]/", "::1 is a loopback address"),
            ("http://[fd00::5]/", "fd00::5 is a unique local address"),
            ("http://[fe80::1]/", "fe80::1 is a link-local address"),
            (
                "http://[::ffff:127.0.0.1]/",
                "::ffff:127.0.0.1 is a loopback address",
            ),
            (
                "https://[::ffff:a9fe:a9fe]/",
                "::ffff:169.254.169.254 is a link-local (cloud metadata) address",
            ),
            (
                "http://0.0.0.0:8080/",
                "0.0.0.0 is a \"this network\" address",
            ),
        ] {
            if msg.is_empty() {
                // a name: checked when it is resolved
                assert!(p.check_url(url).is_ok(), "{url}");
                continue;
            }
            let m = refused(p.check_url(url));
            assert!(m.starts_with(msg), "{url}: {m}");
        }
        assert!(p.check_url("http://8.8.8.8/").is_ok());
        assert!(p.check_url("https://[2606:4700::1111]/").is_ok());
        for url in ["ftp://example.org/x", "file:///etc/passwd", "gopher://h/"] {
            assert!(
                matches!(p.check_url(url), Err(Failure::Failed(m)) if m.contains("only http and https")),
                "{url}"
            );
        }
    }

    #[test]
    fn resolved_names() {
        let p = policy(&[
            ("public.test", &["93.184.216.34", "2606:2800::1"]),
            ("local.test", &["127.0.0.1"]),
            ("lan.test", &["192.168.7.7"]),
            ("metadata.test", &["169.254.169.254"]),
            ("ula.test", &["fd00::1"]),
            ("ll6.test", &["fe80::1"]),
            ("mapped.test", &["::ffff:10.0.0.1"]),
            // one bad address refuses the destination, whatever its position
            ("mixed.test", &["93.184.216.34", "10.0.0.1"]),
            ("mixed6.test", &["::1", "2606:2800::1"]),
            ("empty.test", &[]),
        ]);
        assert_eq!(p.check_host("public.test").unwrap().len(), 2);
        // refused names do not tell what they resolve to
        for (h, ip) in [
            ("local.test", "127.0.0.1"),
            ("lan.test", "192.168.7.7"),
            ("metadata.test", "169.254.169.254"),
            ("ula.test", "fd00::1"),
            ("ll6.test", "fe80::1"),
            ("mapped.test", "10.0.0.1"),
            ("mixed.test", "10.0.0.1"),
            ("mixed6.test", "::1"),
        ] {
            let m = refused(p.check_host(h));
            assert_eq!(m, format!("{h} is refused by the outbound policy"));
            assert!(!m.contains(ip), "{h}: {m}");
        }
        assert!(matches!(
            p.check_host("empty.test"),
            Err(Failure::Failed(m)) if m == "cannot resolve empty.test"
        ));
        assert!(matches!(
            p.check_host("nxdomain.test"),
            Err(Failure::Failed(m)) if m == "cannot resolve nxdomain.test"
        ));
        // allow_private opens private ranges, not link-local ones
        let open = OutboundPolicy {
            allow_private: true,
            ..p.clone()
        };
        for h in [
            "local.test",
            "lan.test",
            "ula.test",
            "mapped.test",
            "mixed.test",
        ] {
            assert!(open.check_host(h).is_ok(), "{h}");
        }
        assert!(refused(open.check_host("metadata.test")).contains("refused"));
        assert!(refused(open.check_host("ll6.test")).contains("refused"));
        assert!(refused(open.check_url("http://169.254.169.254/")).contains("metadata"));
    }

    #[test]
    fn allowlist() {
        let entries = [
            "sparql.example.org",
            "*.lod.example",
            "local.test",
            "10.1.0.0/16",
            "::1",
            "[fd00::7]",
        ];
        let allow: Vec<Allow> = entries.iter().map(|e| e.parse().unwrap()).collect();
        assert_eq!(
            allow,
            [
                Allow::Host("sparql.example.org".into()),
                Allow::Subdomains("lod.example".into()),
                Allow::Host("local.test".into()),
                Allow::Net("10.1.0.0/16".parse().unwrap()),
                Allow::Net("::1/128".parse().unwrap()),
                Allow::Net("fd00::7/128".parse().unwrap()),
            ]
        );
        assert_eq!(
            "SPARQL.Example.ORG.".parse::<Allow>().unwrap(),
            Allow::Host("sparql.example.org".into())
        );
        assert_eq!(
            "127.1".parse::<Allow>().unwrap(),
            Allow::Net("127.0.0.1/32".parse().unwrap())
        );
        for bad in [
            "",
            "host:80",
            "http://x/",
            "*.10.0.0.1",
            "a/b",
            "10.0.0.0/33",
        ] {
            assert!(bad.parse::<Allow>().is_err(), "{bad}");
        }
        let p = OutboundPolicy {
            allow,
            ..policy(&[
                ("sparql.example.org", &["93.184.216.34"]),
                ("local.test", &["127.0.0.1", "::1"]),
                ("v6-local.test", &["::1"]),
                ("a.lod.example", &["93.184.216.35"]),
                ("private.lod.example", &["10.9.9.9"]),
                ("lan.lod.example", &["10.1.2.3"]),
                ("other.example", &["93.184.216.36"]),
                ("lod.example", &["93.184.216.37"]),
                ("in-net.test", &["10.1.200.1"]),
                ("half-in-net.test", &["10.1.200.1", "10.2.0.1"]),
            ])
        };
        // named hosts at public addresses, or at addresses a network entry covers
        assert!(p.check_host("sparql.example.org").is_ok());
        assert!(p.check_host("V6-Local.Test.").is_ok());
        assert_eq!(
            refused(p.check_host("Local.Test.")),
            "local.test is refused by the outbound policy"
        );
        // subdomains, public addresses only (unless covered by a network)
        assert!(p.check_host("a.lod.example").is_ok());
        assert!(
            refused(p.check_host("private.lod.example")).contains("refused by the outbound policy")
        );
        assert!(p.check_host("lan.lod.example").is_ok());
        assert!(refused(p.check_host("lod.example")).contains("not in the outbound allowlist"));
        // names covered by a network, entirely
        assert!(p.check_host("in-net.test").is_ok());
        assert!(
            refused(p.check_host("half-in-net.test")).contains("refused by the outbound policy")
        );
        assert!(refused(p.check_host("other.example")).contains("not in the outbound allowlist"));
        // IP literals
        assert!(p.check_url("http://10.1.3.4:8890/sparql").is_ok());
        assert!(p.check_url("http://[::1]:3030/").is_ok());
        assert!(p.check_url("http://[::ffff:10.1.3.4]/").is_ok());
        assert!(refused(p.check_url("http://10.2.0.1/")).contains("private address"));
        let open = OutboundPolicy {
            allow_private: true,
            ..p.clone()
        };
        assert!(
            refused(open.check_url("http://10.2.0.1/")).contains("not in the outbound allowlist")
        );
        assert!(
            refused(open.check_host("half-in-net.test")).contains("not in the outbound allowlist")
        );
        // with private addresses open, a named host reaches them
        assert!(open.check_host("local.test").is_ok());
        assert!(refused(p.check_url("http://8.8.8.8/")).contains("not in the outbound allowlist"));
        // still only http(s)
        assert!(matches!(
            p.check_url("ftp://sparql.example.org/"),
            Err(Failure::Failed(_))
        ));
        // names only: an unlisted name is refused before any lookup
        let names = OutboundPolicy {
            allow: vec!["sparql.example.org".parse().unwrap()],
            ..Default::default()
        };
        assert!(refused(names.check_url("http://other.example/")).contains("allowlist"));
        assert!(names.check_url("https://sparql.example.org/sparql").is_ok());
    }

    /// A host name in the allowlist vouches for the name, not for the addresses it
    /// resolves to: link-local ones (the metadata service) need a network entry, even
    /// with private addresses open.
    #[test]
    fn allowed_names_do_not_open_link_local_addresses() {
        let base = policy(&[
            ("partner.test", &["169.254.169.254"]),
            ("partner6.test", &["2606:2800::1", "fe80::1"]),
        ]);
        for allow_private in [false, true] {
            let p = OutboundPolicy {
                allow_private,
                allow: vec![
                    "partner.test".parse().unwrap(),
                    "partner6.test".parse().unwrap(),
                ],
                ..base.clone()
            };
            let m = refused(p.check_host("partner.test"));
            assert_eq!(m, "partner.test is refused by the outbound policy");
            assert!(!m.contains("169.254"), "{m}");
            let m = refused(p.check_host("partner6.test"));
            assert!(!m.contains("fe80"), "{m}");
        }
        // the operator lists the address explicitly
        let p = OutboundPolicy {
            allow: vec![
                "partner.test".parse().unwrap(),
                "169.254.169.254".parse().unwrap(),
            ],
            ..base
        };
        assert!(p.check_host("partner.test").is_ok());
    }
}
