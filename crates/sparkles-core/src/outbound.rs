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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// The most requests of one `SERVICE <loop:parallel+n:…>` in flight at once; `parallel+n`
/// is capped to it, and `parallel` alone takes it.
pub const DEFAULT_SERVICE_PARALLEL_MAX: usize = 4;
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
    /// The most requests a `SERVICE <loop:parallel+n:…>` keeps in flight at once;
    /// `parallel+n` is capped to it and `parallel` alone takes it.
    pub service_parallel_max: usize,
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
            service_parallel_max: DEFAULT_SERVICE_PARALLEL_MAX,
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
            .field("service_parallel_max", &self.service_parallel_max)
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

/// An outbound timeout or raw response-size ceiling hit while fetching a response.
/// Carried as a distinct internal error so `LOAD SILENT` does not suppress it while
/// non-silent callers keep the same `Error::Invalid` message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LimitError {
    Timeout(String),
    ResponseTooLarge(String),
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitError::Timeout(m) | LimitError::ResponseTooLarge(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LimitError {}

/// Predicate polled before an outbound request is dispatched: `Some(true)` reports
/// cancellation, `Some(false)` reports abandonment or expiry, and `None` means active.
pub type AbortCheck = Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// Why [`post_json_until_cancellable`] did not receive an HTTP response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostAbort {
    /// Aborted before dispatching the HTTP request because cancellation was observed.
    Cancelled,
    /// Aborted before dispatching the HTTP request because the caller abandoned it or the
    /// absolute deadline expired during setup.
    TimedOutBeforeSend,
    /// Failed during setup or while sending or reading the response.
    Failed(Failure),
}

/// Internal classification of [`OutboundPolicy::send_classified`], distinguishing
/// outbound policy limits (timeouts and over-limit `Content-Length`) from other fetch
/// failures without changing the public [`Failure`] enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendFailure {
    Refused(String),
    Budget(crate::error::Budget),
    Limit(LimitError),
    Failed(String),
}

enum InnerSendFailure {
    Send(SendFailure),
    CancelledBeforeSend,
    TimedOutBeforeSend(String),
}

impl From<Failure> for SendFailure {
    fn from(f: Failure) -> Self {
        match f {
            Failure::Refused(m) => SendFailure::Refused(m),
            Failure::Failed(m) => SendFailure::Failed(m),
            Failure::Budget(b) => SendFailure::Budget(b),
        }
    }
}

impl From<Failure> for InnerSendFailure {
    fn from(f: Failure) -> Self {
        InnerSendFailure::Send(f.into())
    }
}

impl From<SendFailure> for InnerSendFailure {
    fn from(f: SendFailure) -> Self {
        InnerSendFailure::Send(f)
    }
}

impl SendFailure {
    fn into_failure(self) -> Failure {
        match self {
            SendFailure::Refused(m) => Failure::Refused(m),
            SendFailure::Budget(b) => Failure::Budget(b),
            SendFailure::Limit(l) => Failure::Failed(l.to_string()),
            SendFailure::Failed(m) => Failure::Failed(m),
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
///
/// A request that runs beside others of the same SPARQL request can take a
/// [`share`](Self::share) of the time left: a budget of its own whose time is reserved
/// in this one until the share ends, so that the requests in flight together cannot take
/// more time than this budget has, and whose bytes this budget counts. The time spent
/// and the time reserved change together under one lock, so a reader never sees a
/// share's reservation released before its time is spent, and two shares taken at once
/// never reserve the same time. A single-request dispatch (one without a share) draws
/// on the parent directly and snapshots the remaining time at dispatch: concurrent
/// direct dispatches on the same parent can each see the full time left and together
/// exceed it. Use shares for concurrent requests that must stay within a cumulative
/// limit.
#[derive(Debug)]
pub struct RequestBudget {
    max_bytes: u64,
    max_time: Duration,
    bytes: AtomicU64,
    time: Mutex<Ledger>,
    /// for a share, the budget it was taken from, with the nanoseconds it reserved there
    parent: Option<(Arc<RequestBudget>, u64)>,
}

/// The time of a [`RequestBudget`], in nanoseconds.
#[derive(Debug, Default)]
struct Ledger {
    /// taken by requests that ended, and by shares that ended
    spent: u64,
    /// reserved by shares in flight
    reserved: u64,
}

impl Ledger {
    fn used(&self) -> u64 {
        self.spent.saturating_add(self.reserved)
    }
}

impl RequestBudget {
    /// A fresh budget under `policy`'s totals.
    pub fn new(policy: &OutboundPolicy) -> Arc<RequestBudget> {
        Arc::new(RequestBudget {
            max_bytes: policy.max_request_bytes,
            max_time: policy.request_timeout,
            bytes: AtomicU64::new(0),
            time: Mutex::new(Ledger::default()),
            parent: None,
        })
    }

    /// A share of at most `want` of the time this budget has left, reserved here until
    /// the share is dropped, when the time it took is spent here instead. `None` when no
    /// time is left. The share's bytes are this budget's.
    pub(crate) fn share(self: &Arc<Self>, want: Duration) -> Option<Arc<RequestBudget>> {
        let want = u64::try_from(want.as_nanos()).unwrap_or(u64::MAX);
        let max = u64::try_from(self.max_time.as_nanos()).unwrap_or(u64::MAX);
        let take = {
            let mut time = self.time.lock();
            let take = want.min(max.saturating_sub(time.used()));
            time.reserved = time.reserved.saturating_add(take);
            take
        };
        (take > 0).then(|| {
            Arc::new(RequestBudget {
                max_bytes: self.max_bytes,
                max_time: Duration::from_nanos(take),
                bytes: AtomicU64::new(0),
                time: Mutex::new(Ledger::default()),
                parent: Some((self.clone(), take)),
            })
        })
    }

    /// The budget that counts the bytes: this one, or the one a share was taken from.
    fn root(&self) -> &RequestBudget {
        match &self.parent {
            Some((p, _)) => p.root(),
            None => self,
        }
    }

    /// The total time of the SPARQL request's outbound requests.
    fn total_time(&self) -> Duration {
        self.root().max_time
    }

    /// Bytes received so far.
    pub fn bytes(&self) -> u64 {
        self.root().bytes.load(Ordering::Relaxed)
    }

    /// Time the requests took so far, summed.
    pub fn time(&self) -> Duration {
        Duration::from_nanos(self.time.lock().spent)
    }

    /// The time the budget has left, without what its shares in flight reserve.
    pub fn remaining_time(&self) -> Duration {
        let used = self.time.lock().used();
        self.max_time.saturating_sub(Duration::from_nanos(used))
    }

    fn remaining_bytes(&self) -> u64 {
        let root = self.root();
        root.max_bytes.saturating_sub(root.bytes())
    }

    fn spend_time(&self, d: Duration) {
        let n = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let mut time = self.time.lock();
        time.spent = time.spent.saturating_add(n);
    }

    /// Count `n` more bytes; the exceeded budget once past the total.
    fn spend_bytes(&self, n: u64) -> Result<(), crate::error::Budget> {
        let root = self.root();
        let used = root.bytes.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        if used > root.max_bytes {
            return Err(root.exceeded(used));
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

impl Drop for RequestBudget {
    /// A share gives its reservation back and spends the time it took, in one step.
    fn drop(&mut self) {
        if let Some((p, take)) = &self.parent {
            let took = self.time.get_mut().spent;
            let mut time = p.time.lock();
            time.spent = time.spent.saturating_add(took);
            time.reserved = time.reserved.saturating_sub(*take);
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
        self.send_classified(budget, url, timeout, build)
            .map_err(SendFailure::into_failure)
    }

    /// Like [`send`](Self::send), bounded by an absolute `deadline` rather than a
    /// relative duration.
    pub(crate) fn send_until(
        &self,
        budget: &Arc<RequestBudget>,
        url: &str,
        deadline: Instant,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, Failure> {
        self.send_classified_until(budget, url, deadline, build)
            .map_err(SendFailure::into_failure)
    }

    /// [`send`](Self::send) preserving whether a failure was an outbound timeout or
    /// over-limit `Content-Length` rather than a general fetch error.
    pub(crate) fn send_classified(
        &self,
        budget: &Arc<RequestBudget>,
        url: &str,
        timeout: Duration,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, SendFailure> {
        let own = timeout.min(self.timeout);
        self.send_classified_until(budget, url, Instant::now() + own, build)
    }

    /// [`send_classified`](Self::send_classified) bounded by an absolute `deadline` that
    /// is checked before URL validation, immediately before sending, and during response
    /// reads so setup or scheduling delays never extend it.
    pub(crate) fn send_classified_until(
        &self,
        budget: &Arc<RequestBudget>,
        url: &str,
        deadline: Instant,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, SendFailure> {
        self.send_classified_until_with_abort(budget, url, deadline, None, build)
            .map_err(|e| match e {
                InnerSendFailure::Send(sf) => sf,
                InnerSendFailure::CancelledBeforeSend => {
                    SendFailure::Failed("the request was cancelled".into())
                }
                InnerSendFailure::TimedOutBeforeSend(m) => {
                    SendFailure::Limit(LimitError::Timeout(m))
                }
            })
    }

    fn send_classified_until_with_abort(
        &self,
        budget: &Arc<RequestBudget>,
        url: &str,
        deadline: Instant,
        abort: Option<AbortCheck>,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, InnerSendFailure> {
        #[cfg(any(test, feature = "failpoints"))]
        if probe::intercept(url) {
            return Err(SendFailure::Failed("owned test refused outbound send".into()).into());
        }
        let check_abort = |abort: &Option<AbortCheck>| -> Result<(), InnerSendFailure> {
            if let Some(cancelled) = abort.as_deref().and_then(|f| f()) {
                return Err(if cancelled {
                    InnerSendFailure::CancelledBeforeSend
                } else {
                    InnerSendFailure::TimedOutBeforeSend(timed_out(Duration::ZERO))
                });
            }
            Ok(())
        };
        check_abort(&abort)?;
        let own = deadline
            .saturating_duration_since(Instant::now())
            .min(self.timeout);
        if own.is_zero() {
            return Err(InnerSendFailure::TimedOutBeforeSend(timed_out(
                Duration::ZERO,
            )));
        }
        let u = self.check_url(url)?;
        check_abort(&abort)?;
        let left = budget.remaining_time();
        if left.is_zero() {
            return Err(InnerSendFailure::TimedOutBeforeSend(out_of_time(
                budget.total_time(),
            )));
        }
        let failed = Arc::new(Mutex::new(None));
        let aborted_before_connect = Arc::new(Mutex::new(None));
        let initial_dns_in_flight = Arc::new(AtomicBool::new(false));
        let redirected = Arc::new(AtomicBool::new(false));
        let client = self
            .client(
                &failed,
                aborted_before_connect.clone(),
                initial_dns_in_flight.clone(),
                redirected.clone(),
                abort.clone(),
                deadline,
            )
            // a client that cannot be built is a local configuration error (such as an
            // `SSL_CERT_FILE` that names no file), so its cause is safe to show
            .map_err(|e| {
                SendFailure::Failed(format!(
                    "{}: {}",
                    hidden(url, "the HTTP client could not be built", &e),
                    chain(&e)
                ))
            })?;
        // Build the request and run the outbound headers hook BEFORE the final dispatch
        // checks and remaining-timeout calculation so pauses in setup or the hook cannot
        // slip past cancellation or the absolute deadline.
        let mut req = apply(build(&client, u))
            .build()
            .map_err(|e| SendFailure::Failed(hidden(url, "the request could not be built", &e)))?;
        #[cfg(any(test, feature = "failpoints"))]
        probe::pause_before_send(url);
        check_abort(&abort)?;
        let start = Instant::now();
        let own_now = deadline.saturating_duration_since(start).min(self.timeout);
        let left_now = budget.remaining_time();
        let remaining_now = own_now.min(left_now);
        // the message of a timeout: this request's, or the whole SPARQL request's
        let late = if left_now < own_now {
            out_of_time(budget.total_time())
        } else {
            timed_out(remaining_now)
        };
        if remaining_now.is_zero() {
            return Err(InnerSendFailure::TimedOutBeforeSend(late));
        }
        *req.timeout_mut() = Some(remaining_now);
        let resp = client.execute(req);
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                budget.spend_time(start.elapsed());
                if !redirected.load(Ordering::SeqCst) {
                    if let Some(cancelled) = aborted_before_connect.lock().take() {
                        return Err(if cancelled {
                            InnerSendFailure::CancelledBeforeSend
                        } else {
                            InnerSendFailure::TimedOutBeforeSend(late)
                        });
                    }
                    if initial_dns_in_flight.load(Ordering::SeqCst)
                        && (e.is_timeout()
                            || start.elapsed() >= remaining_now
                            || Instant::now() >= deadline)
                    {
                        return Err(if abort.as_deref().and_then(|f| f()) == Some(true) {
                            InnerSendFailure::CancelledBeforeSend
                        } else {
                            InnerSendFailure::TimedOutBeforeSend(late)
                        });
                    }
                }
                // a refused or unresolvable name, in words fit for the caller
                if let Some(f) = failed.lock().take() {
                    return Err(f.into());
                }
                if e.is_timeout() || start.elapsed() >= remaining_now || Instant::now() >= deadline
                {
                    return Err(SendFailure::Limit(LimitError::Timeout(late)).into());
                }
                // redirect errors are the policy's own words (hops, schemes)
                if e.is_redirect() {
                    return Err(SendFailure::Failed(chain(&e)).into());
                }
                let what = if e.is_connect() && chain(&e).contains("invalid peer certificate") {
                    "cannot connect: the server's TLS certificate is not trusted"
                } else if e.is_connect() {
                    "cannot connect"
                } else {
                    "the request failed"
                };
                return Err(SendFailure::Failed(hidden(url, what, &e)).into());
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
            deadline: (start + remaining_now).min(deadline),
            late,
        };
        if let Some(n) = body.resp.content_length() {
            if n > limit {
                return Err(
                    SendFailure::Limit(LimitError::ResponseTooLarge(too_large(limit))).into(),
                );
            }
            if n > budget.remaining_bytes() {
                return Err(SendFailure::Budget(budget.exceeded(budget.bytes() + n)).into());
            }
        }
        let content_type = body
            .resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let etag = body
            .resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let status = body.resp.status();
        Ok(Response {
            status,
            content_type,
            etag,
            body,
        })
    }

    /// A client that resolves (and so connects) only through the policy, checks every
    /// redirect, and records a refusal or a failed lookup in `failed`.
    fn client(
        &self,
        failed: &Arc<Mutex<Option<Failure>>>,
        aborted: Arc<Mutex<Option<bool>>>,
        initial_dns_in_flight: Arc<AtomicBool>,
        redirected: Arc<AtomicBool>,
        abort: Option<AbortCheck>,
        deadline: Instant,
    ) -> reqwest::Result<reqwest::blocking::Client> {
        let redirect = self.redirect_policy(failed, redirected.clone());
        let mut b = reqwest::blocking::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .redirect(redirect)
            // a proxy would resolve and connect on its own, past the checks
            .no_proxy()
            .dns_resolver(Arc::new(Pinned {
                policy: self.clone(),
                failed: failed.clone(),
                aborted,
                initial_dns_in_flight,
                redirected,
                abort,
                deadline,
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
    fn redirect_policy(
        &self,
        failed: &Arc<Mutex<Option<Failure>>>,
        redirected: Arc<AtomicBool>,
    ) -> reqwest::redirect::Policy {
        let (p, slot, max) = (self.clone(), failed.clone(), self.max_redirects);
        let custom = self.tls.is_custom();
        reqwest::redirect::Policy::custom(move |a| {
            // Receiving any redirect proves an earlier hop already reached a server,
            // so later DNS/connect aborts must never be classified as before-send.
            redirected.store(true, Ordering::SeqCst);
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
        })
    }

    /// Open a download through the same checks as the blocking outbound client. The
    /// caller owns the response budget and may drop either a pending send or a body
    /// read to abort it, including while the connection is idle.
    pub async fn open_download(
        &self,
        url: &str,
        deadline: Instant,
    ) -> Result<reqwest::Response, Failure> {
        #[cfg(any(test, feature = "failpoints"))]
        if probe::intercept(url) {
            return Err(Failure::Failed("owned test refused outbound send".into()));
        }
        let u = self.check_url(url)?;
        let failed = Arc::new(Mutex::new(None));
        let redirected = Arc::new(AtomicBool::new(false));
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .redirect(self.redirect_policy(&failed, redirected.clone()))
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .dns_resolver(Arc::new(Pinned {
                policy: self.clone(),
                failed: failed.clone(),
                aborted: Arc::default(),
                initial_dns_in_flight: Arc::default(),
                redirected,
                abort: None,
                deadline,
            }))
            .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")));
        let build_error = |e: reqwest::Error| {
            Failure::Failed(hidden(url, "the HTTP client could not be built", &e))
        };
        if let Some(pem) = &self.tls.extra_roots_pem {
            builder = builder
                .tls_certs_merge(reqwest::Certificate::from_pem_bundle(pem).map_err(build_error)?);
        }
        if self.tls.insecure_skip_verify {
            builder = builder.tls_danger_accept_invalid_certs(true);
        }
        let client = builder.build().map_err(build_error)?;
        let mut request = client.get(u).header("Accept", "*/*");
        if let Some(hook) = HOOK.get() {
            let mut headers = Vec::new();
            hook(&mut |k, v| headers.push((k.to_string(), v.to_string())));
            for (name, value) in headers {
                request = request.header(name, value);
            }
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(self.timeout);
        if remaining.is_zero() {
            return Err(Failure::Failed(timed_out(remaining)));
        }
        request.timeout(remaining).send().await.map_err(|e| {
            failed
                .lock()
                .take()
                .unwrap_or_else(|| Failure::Failed(hidden(url, "the download request failed", &e)))
        })
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The resolver of a request's client: resolves through the policy and hands the
/// checked addresses, and only those, to the connector.
struct Pinned {
    policy: OutboundPolicy,
    failed: Arc<Mutex<Option<Failure>>>,
    aborted: Arc<Mutex<Option<bool>>>,
    initial_dns_in_flight: Arc<AtomicBool>,
    redirected: Arc<AtomicBool>,
    abort: Option<AbortCheck>,
    deadline: Instant,
}

impl reqwest::dns::Resolve for Pinned {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (policy, failed, aborted, initial_dns_in_flight, redirected, abort, deadline) = (
            self.policy.clone(),
            self.failed.clone(),
            self.aborted.clone(),
            self.initial_dns_in_flight.clone(),
            self.redirected.clone(),
            self.abort.clone(),
            self.deadline,
        );
        let host = name.as_str().to_string();
        Box::pin(async move {
            let is_initial = !redirected.load(Ordering::SeqCst);
            if is_initial {
                initial_dns_in_flight.store(true, Ordering::SeqCst);
            }
            let check = || -> Result<(), BoxError> {
                if let Some(cancelled) = abort.as_deref().and_then(|f| f()) {
                    if is_initial && !redirected.load(Ordering::SeqCst) {
                        *aborted.lock() = Some(cancelled);
                    }
                    return Err("outbound request aborted before connect".into());
                }
                if Instant::now() >= deadline {
                    if is_initial && !redirected.load(Ordering::SeqCst) {
                        *aborted.lock() = Some(false);
                    }
                    return Err("outbound deadline expired before connect".into());
                }
                Ok(())
            };
            check()?;
            let checked = tokio::task::spawn_blocking(move || policy.check_host(&host)).await?;
            check()?;
            // Initial DNS completed before any deadline/abort: clear before handing
            // addresses to the connector (or reporting a DNS/policy failure).
            if is_initial {
                initial_dns_in_flight.store(false, Ordering::SeqCst);
            }
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

/// A document [`fetch_document`] read.
#[derive(Clone, Debug)]
pub struct Document {
    pub bytes: Vec<u8>,
    pub content_type: String,
    /// the response's `ETag`
    pub etag: Option<String>,
}

/// GET `url` through `policy` as [`fetch_bytes`] does, and return the body with its
/// `Content-Type` and `ETag` (the explicit import fetch of spec U12).
pub fn fetch_document(
    policy: &OutboundPolicy,
    budget: &Arc<RequestBudget>,
    url: &str,
    accept: &str,
) -> crate::Result<Document> {
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
    let etag = resp.etag.clone();
    let mut bytes = Vec::new();
    let mut body = resp.body;
    body.read_to_end(&mut bytes)
        .map_err(|e| match crate::codec::io_error(e) {
            crate::Error::Io(e) => failed(e.to_string()),
            e => e,
        })?;
    Ok(Document {
        bytes,
        content_type,
        etag,
    })
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
    let deadline = Instant::now() + timeout.min(policy.timeout);
    post_json_until(policy, url, headers, body, deadline)
}

/// Like [`post_json`], bounded by an absolute `deadline` rather than a relative duration.
pub fn post_json_until(
    policy: &OutboundPolicy,
    url: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    deadline: Instant,
) -> Result<Posted, Failure> {
    let budget = RequestBudget::new(policy);
    let resp = policy.send_until(&budget, url, deadline, |client, u| {
        let mut rb = client
            .post(u)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.body(body)
    })?;
    read_posted(resp).map_err(Failure::Failed)
}

/// Like [`post_json_until`], polling `abort` across setup and immediately before
/// dispatching the HTTP request so cancellation, abandonment, or deadline expiry during
/// URL validation, TLS client construction, request construction, header hooks, or DNS
/// resolution aborts cleanly without sending the POST.
pub fn post_json_until_cancellable(
    policy: &OutboundPolicy,
    url: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    deadline: Instant,
    abort: Option<AbortCheck>,
) -> Result<Posted, PostAbort> {
    let budget = RequestBudget::new(policy);
    let resp = policy
        .send_classified_until_with_abort(&budget, url, deadline, abort, |client, u| {
            let mut rb = client
                .post(u)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            for (k, v) in headers {
                rb = rb.header(*k, *v);
            }
            rb.body(body)
        })
        .map_err(|e| match e {
            InnerSendFailure::CancelledBeforeSend => PostAbort::Cancelled,
            InnerSendFailure::TimedOutBeforeSend(_) => PostAbort::TimedOutBeforeSend,
            InnerSendFailure::Send(other) => PostAbort::Failed(other.into_failure()),
        })?;
    read_posted(resp).map_err(|e| PostAbort::Failed(Failure::Failed(e)))
}

fn read_posted(resp: Response) -> Result<Posted, String> {
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
    b.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
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
    /// the `ETag` header, when it is text
    pub etag: Option<String>,
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
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                LimitError::Timeout(self.late.clone()),
            ));
        }
        let n = match self.resp.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut || late(self.deadline) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    LimitError::Timeout(self.late.clone()),
                ));
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
            return Err(io::Error::other(LimitError::ResponseTooLarge(too_large(
                self.limit,
            ))));
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

/// URL-scoped outbound observers, available only with the existing test failpoints.
/// Matching sends are counted and refused before URL resolution or network access.
#[cfg(any(test, feature = "failpoints"))]
#[doc(hidden)]
pub mod probe {
    use super::*;
    use std::collections::BTreeMap;

    type PauseFn = Arc<dyn Fn() + Send + Sync>;

    static SENDS: OnceLock<Mutex<BTreeMap<String, Arc<AtomicU64>>>> = OnceLock::new();
    static BEFORE_SEND: OnceLock<Mutex<BTreeMap<String, PauseFn>>> = OnceLock::new();

    pub struct SendProbe {
        url: String,
        entries: Arc<AtomicU64>,
    }

    impl SendProbe {
        pub fn install(url: String) -> Self {
            let entries = Arc::new(AtomicU64::new(0));
            let mut sends = SENDS.get_or_init(Default::default).lock();
            assert!(!sends.contains_key(&url), "duplicate outbound probe URL");
            sends.insert(url.clone(), entries.clone());
            Self { url, entries }
        }

        pub fn entries(&self) -> u64 {
            self.entries.load(Ordering::SeqCst)
        }
    }

    impl Drop for SendProbe {
        fn drop(&mut self) {
            if let Some(sends) = SENDS.get() {
                sends.lock().remove(&self.url);
            }
        }
    }

    /// URL-scoped hook invoked after request construction and the outbound headers hook,
    /// immediately before the final abort/deadline dispatch check and `client.execute`.
    pub struct BeforeSendHook {
        url: String,
    }

    impl BeforeSendHook {
        pub fn install(url: String, hook: impl Fn() + Send + Sync + 'static) -> Self {
            let mut map = BEFORE_SEND.get_or_init(Default::default).lock();
            assert!(!map.contains_key(&url), "duplicate before-send hook URL");
            map.insert(url.clone(), Arc::new(hook));
            Self { url }
        }
    }

    impl Drop for BeforeSendHook {
        fn drop(&mut self) {
            if let Some(map) = BEFORE_SEND.get() {
                map.lock().remove(&self.url);
            }
        }
    }

    pub(super) fn intercept(url: &str) -> bool {
        let Some(sends) = SENDS.get() else {
            return false;
        };
        let sends = sends.lock();
        let Some(entries) = sends.get(url) else {
            return false;
        };
        entries.fetch_add(1, Ordering::SeqCst);
        true
    }

    pub(super) fn pause_before_send(url: &str) {
        let hook = BEFORE_SEND
            .get()
            .and_then(|map| map.lock().get(url).cloned());
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A share reserves its time in the budget it was taken from until it ends, so a
    /// second user of the budget sees only what is left, and the time a share took is
    /// spent there once it ends. Its bytes are the budget's.
    #[test]
    fn shares_reserve_their_time_and_count_their_bytes_in_the_budget() {
        let b = RequestBudget::new(&OutboundPolicy {
            request_timeout: Duration::from_secs(10),
            max_request_bytes: 100,
            ..Default::default()
        });
        let a = b.share(Duration::from_secs(4)).unwrap();
        assert_eq!(a.remaining_time(), Duration::from_secs(4));
        assert_eq!(b.remaining_time(), Duration::from_secs(6));
        // a second user takes what is left, and no more
        let c = b.share(Duration::from_secs(8)).unwrap();
        assert_eq!(c.remaining_time(), Duration::from_secs(6));
        assert!(b.remaining_time().is_zero());
        assert!(b.share(Duration::from_secs(1)).is_none(), "nothing is left");
        // bytes are the budget's, for every share
        a.spend_bytes(60).unwrap();
        assert_eq!((b.bytes(), c.bytes()), (60, 60));
        assert!(c.spend_bytes(50).is_err(), "past the budget's bytes");
        // a share that ends gives back what it did not take
        a.spend_time(Duration::from_secs(1));
        drop(a);
        assert_eq!(b.remaining_time(), Duration::from_secs(3));
        assert_eq!(b.time(), Duration::from_secs(1));
        drop(c);
        assert_eq!(b.remaining_time(), Duration::from_secs(9));
        assert_eq!(b.total_time(), Duration::from_secs(10));
    }

    /// Shares that spend all the time they reserve leave the time used unchanged when
    /// they end, so the time a budget has left never rises while they are taken and
    /// ended by many threads at once. A reader that saw what a share spent and what it
    /// reserved at different moments would see the time left rise for that moment.
    #[test]
    fn concurrent_shares_keep_one_ledger() {
        use std::sync::atomic::AtomicBool;
        let total = Duration::from_secs(1_000_000);
        let b = RequestBudget::new(&OutboundPolicy {
            request_timeout: total,
            ..Default::default()
        });
        let each = Duration::from_millis(3);
        let (writers, rounds) = (4u32, 20_000u32);
        let done = AtomicBool::new(false);
        let ended = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let (b, done) = (b.clone(), &done);
                scope.spawn(move || {
                    let mut last = b.remaining_time();
                    while !done.load(Ordering::Acquire) {
                        let left = b.remaining_time();
                        assert!(left <= last, "the time left rose from {last:?} to {left:?}");
                        last = left;
                    }
                });
            }
            let mut ws = Vec::new();
            for _ in 0..writers {
                let (b, ended) = (b.clone(), &ended);
                ws.push(scope.spawn(move || {
                    for _ in 0..rounds {
                        let s = b.share(each).expect("time is left");
                        assert!(s.remaining_time() <= each);
                        s.spend_time(each);
                        drop(s);
                        ended.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            for w in ws {
                w.join().unwrap();
            }
            done.store(true, Ordering::Release);
        });
        let n = ended.load(Ordering::Relaxed);
        assert_eq!(n, writers * rounds);
        assert_eq!(b.time(), each * n);
        assert_eq!(b.remaining_time(), total - each * n);
    }
    use std::collections::HashMap;

    #[test]
    fn authoring_consult_outbound_probe_counts_send_before_network() {
        let url = "http://127.0.0.1:1/authoring-consult-probe-control";
        let probe = probe::SendProbe::install(url.into());
        let policy = OutboundPolicy::default();
        let result = policy.send(
            &RequestBudget::new(&policy),
            url,
            Duration::from_secs(1),
            |client, url| client.get(url),
        );
        assert!(
            matches!(result, Err(Failure::Failed(ref e)) if e == "owned test refused outbound send")
        );
        assert_eq!(probe.entries(), 1);
    }

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

    #[test]
    fn dns_deadline_classification_distinguishes_initial_and_redirect_hops() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::AtomicUsize;

        struct RedirectServer {
            port: u16,
            hop1_hits: Arc<AtomicUsize>,
            hop2_hits: Arc<AtomicUsize>,
            stop: Arc<AtomicBool>,
        }
        impl Drop for RedirectServer {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::SeqCst);
                let _ = TcpStream::connect(("127.0.0.1", self.port));
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect server");
        let port = listener.local_addr().unwrap().port();
        let hop1_hits = Arc::new(AtomicUsize::new(0));
        let hop2_hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let hop1_hits = hop1_hits.clone();
            let hop2_hits = hop2_hits.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut stream) = conn else {
                        continue;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut req_line = String::new();
                    if reader.read_line(&mut req_line).is_err() || req_line.is_empty() {
                        continue;
                    }
                    let mut content_len = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err()
                            || line == "\r\n"
                            || line == "\n"
                            || line.is_empty()
                        {
                            break;
                        }
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    if content_len > 0 {
                        let mut body = vec![0u8; content_len];
                        let _ = reader.read_exact(&mut body);
                    }
                    if req_line.contains("/hop2") {
                        hop2_hits.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        );
                    } else {
                        hop1_hits.fetch_add(1, Ordering::SeqCst);
                        let resp = format!(
                            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://redirect.test:{port}/hop2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    let _ = stream.flush();
                }
            });
        }
        let server = RedirectServer {
            port,
            hop1_hits,
            hop2_hits,
            stop,
        };

        struct ControlledResolver {
            pause_host: &'static str,
            deadline: Instant,
            paused: Arc<AtomicBool>,
        }
        impl Resolver for ControlledResolver {
            fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
                if host.eq_ignore_ascii_case(self.pause_host) {
                    self.paused.store(true, Ordering::SeqCst);
                    while Instant::now() < self.deadline {
                        let rem = self.deadline.saturating_duration_since(Instant::now());
                        std::thread::sleep(rem + Duration::from_millis(15));
                    }
                }
                Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
            }
        }

        // 1. Expiry during initial DNS must report TimedOutBeforeSend with zero hits.
        let dl1 = Instant::now() + Duration::from_millis(400);
        let paused1 = Arc::new(AtomicBool::new(false));
        let p1 = OutboundPolicy {
            allow_private: true,
            resolver: Arc::new(ControlledResolver {
                pause_host: "initial.test",
                deadline: dl1,
                paused: paused1.clone(),
            }),
            ..Default::default()
        };
        let r1 = post_json_until_cancellable(
            &p1,
            &format!("http://initial.test:{}/hop1", server.port),
            &[],
            b"{}".to_vec(),
            dl1,
            None,
        );
        assert!(paused1.load(Ordering::SeqCst), "initial resolver must run");
        assert!(
            matches!(r1, Err(PostAbort::TimedOutBeforeSend)),
            "initial DNS expiry must classify as TimedOutBeforeSend, got: {:?}",
            r1.err()
        );
        assert_eq!(server.hop1_hits.load(Ordering::SeqCst), 0);
        assert_eq!(server.hop2_hits.load(Ordering::SeqCst), 0);

        // 2. Expiry during redirect DNS must NOT report TimedOutBeforeSend because hop 1
        // already received the POST.
        let dl2 = Instant::now() + Duration::from_millis(400);
        let paused2 = Arc::new(AtomicBool::new(false));
        let p2 = OutboundPolicy {
            allow_private: true,
            resolver: Arc::new(ControlledResolver {
                pause_host: "redirect.test",
                deadline: dl2,
                paused: paused2.clone(),
            }),
            ..Default::default()
        };
        let r2 = post_json_until_cancellable(
            &p2,
            &format!("http://hop1.test:{}/hop1", server.port),
            &[],
            b"{}".to_vec(),
            dl2,
            None,
        );
        assert!(paused2.load(Ordering::SeqCst), "redirect resolver must run");
        assert!(
            matches!(r2, Err(PostAbort::Failed(_))),
            "redirect DNS expiry after sent hop must classify as Failed (unknown), got: {:?}",
            r2.err()
        );
        assert_eq!(server.hop1_hits.load(Ordering::SeqCst), 1);
        assert_eq!(server.hop2_hits.load(Ordering::SeqCst), 0);
    }
}
