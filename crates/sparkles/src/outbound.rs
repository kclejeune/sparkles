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
//!   while it streams in.
//!
//! By default only public unicast addresses may be contacted: loopback, private
//! (RFC 1918), shared (100.64.0.0/10), link-local (among them the 169.254.169.254
//! metadata service), unique-local, multicast and the other special-purpose ranges are
//! refused, also in their IPv4-mapped, IPv4-compatible, NAT64 and 6to4 IPv6 forms.
//! [`OutboundPolicy::allow_private`] and [`OutboundPolicy::allow`] open them up.
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
    /// This host name; it may resolve to any address, private ones included (the
    /// entry vouches for the name).
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
    /// [`OutboundPolicy::allow_private`] or an allowlist entry that covers them.
    Private(&'static str),
    /// Link-local (the cloud metadata service), multicast, broadcast, unspecified and
    /// reserved addresses: contacted only with an allowlist entry that covers them.
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
    /// When non-empty, only these destinations are contacted. A [`Allow::Host`] or
    /// [`Allow::Net`] entry also admits non-public addresses; [`Allow::Subdomains`]
    /// does not.
    pub allow: Vec<Allow>,
    /// Time to establish a connection.
    pub connect_timeout: Duration,
    /// Total time of a request, until the end of the response body.
    pub timeout: Duration,
    /// Ceiling on the response body, in bytes (decompressed, for a compressed `LOAD`).
    pub max_response_bytes: u64,
    /// Redirects followed; every hop is checked like the first URL.
    pub max_redirects: usize,
    pub resolver: Arc<dyn Resolver>,
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
            resolver: Arc::new(SystemResolver),
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
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(m) | Failure::Failed(m) => f.write_str(m),
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
        let addrs = self
            .resolver
            .resolve(host)
            .map_err(|e| Failure::Failed(format!("cannot resolve {host}: {e}")))?;
        if addrs.is_empty() {
            return Err(Failure::Failed(format!("{host} has no addresses")));
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
    fn check_addrs(&self, host: Option<&str>, addrs: &[IpAddr]) -> Result<(), Failure> {
        let h = host.map(|h| h.trim_end_matches('.').to_ascii_lowercase());
        if let Some(h) = &h
            && self.allow.contains(&Allow::Host(h.clone()))
        {
            return Ok(());
        }
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
                        Some(h) => format!("{h} resolves to {ip}, a {what} address"),
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

    /// Send the request `build` makes for `url`, within `timeout` (at most the policy's),
    /// and return the response once its headers are in.
    pub(crate) fn send(
        &self,
        url: &str,
        timeout: Duration,
        build: impl FnOnce(&reqwest::blocking::Client, Url) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Response, Failure> {
        let u = self.check_url(url)?;
        let timeout = timeout.min(self.timeout);
        let refused = Arc::new(Mutex::new(None));
        let client = self
            .client(&refused)
            .map_err(|e| Failure::Failed(chain(&e)))?;
        let start = Instant::now();
        let resp = apply(build(&client, u)).timeout(timeout).send();
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if let Some(m) = refused.lock().take() {
                    return Err(Failure::Refused(m));
                }
                if e.is_timeout() || start.elapsed() >= timeout {
                    return Err(Failure::Failed(timed_out(timeout)));
                }
                return Err(Failure::Failed(chain(&e)));
            }
        };
        let limit = self.max_response_bytes;
        if resp.content_length().is_some_and(|n| n > limit) {
            return Err(Failure::Failed(too_large(limit)));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        Ok(Response {
            status: resp.status(),
            content_type,
            body: Body {
                resp,
                read: 0,
                limit,
                deadline: start + timeout,
                timeout,
            },
        })
    }

    /// A client that resolves (and so connects) only through the policy, checks every
    /// redirect, and records a refusal in `refused`.
    fn client(
        &self,
        refused: &Arc<Mutex<Option<String>>>,
    ) -> reqwest::Result<reqwest::blocking::Client> {
        let (p, slot, max) = (self.clone(), refused.clone(), self.max_redirects);
        let redirect = reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() > max {
                return a.error(format!("more than {max} redirects"));
            }
            match p.check_parsed(a.url()) {
                Ok(()) => a.follow(),
                Err(f) => {
                    let m = format!("redirect to {}: {f}", a.url());
                    if matches!(f, Failure::Refused(_)) {
                        *slot.lock() = Some(m.clone());
                    }
                    a.error(m)
                }
            }
        });
        reqwest::blocking::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .redirect(redirect)
            // a proxy would resolve and connect on its own, past the checks
            .no_proxy()
            .dns_resolver(Arc::new(Pinned {
                policy: self.clone(),
                refused: refused.clone(),
            }))
            .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")))
            .build()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The resolver of a request's client: resolves through the policy and hands the
/// checked addresses, and only those, to the connector.
struct Pinned {
    policy: OutboundPolicy,
    refused: Arc<Mutex<Option<String>>>,
}

impl reqwest::dns::Resolve for Pinned {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (policy, refused) = (self.policy.clone(), self.refused.clone());
        let host = name.as_str().to_string();
        Box::pin(async move {
            let checked = tokio::task::spawn_blocking(move || policy.check_host(&host)).await?;
            let r: Result<reqwest::dns::Addrs, BoxError> = match checked {
                Ok(addrs) => Ok(Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)))),
                Err(f) => {
                    if let Failure::Refused(m) = &f {
                        *refused.lock() = Some(m.clone());
                    }
                    Err(f.to_string().into())
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

/// A response whose headers are in.
pub(crate) struct Response {
    pub status: reqwest::StatusCode,
    pub content_type: String,
    pub body: Body,
}

/// A response body under the policy's byte ceiling and deadline.
pub(crate) struct Body {
    resp: reqwest::blocking::Response,
    read: u64,
    limit: u64,
    deadline: Instant,
    timeout: Duration,
}

impl Body {
    /// The whole body.
    pub fn bytes(mut self) -> Result<Vec<u8>, Failure> {
        let mut out = Vec::new();
        self.read_to_end(&mut out)
            .map_err(|e| Failure::Failed(e.to_string()))?;
        Ok(out)
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let late = |d: Instant| Instant::now() >= d;
        if late(self.deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                timed_out(self.timeout),
            ));
        }
        let n = match self.resp.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut || late(self.deadline) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    timed_out(self.timeout),
                ));
            }
            Err(e) => return Err(e),
        };
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other(too_large(self.limit)));
        }
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
        for (h, msg) in [
            (
                "local.test",
                "local.test resolves to 127.0.0.1, a loopback address",
            ),
            ("lan.test", "a private address"),
            ("metadata.test", "a link-local (cloud metadata) address"),
            ("ula.test", "a unique local address"),
            ("ll6.test", "a link-local address"),
            ("mapped.test", "::ffff:10.0.0.1, a private address"),
            ("mixed.test", "10.0.0.1, a private address"),
            ("mixed6.test", "::1, a loopback address"),
        ] {
            let m = refused(p.check_host(h));
            assert!(m.contains(msg), "{h}: {m}");
        }
        assert!(matches!(
            p.check_host("empty.test"),
            Err(Failure::Failed(_))
        ));
        assert!(matches!(
            p.check_host("nxdomain.test"),
            Err(Failure::Failed(m)) if m.contains("cannot resolve")
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
        assert!(refused(open.check_host("metadata.test")).contains("link-local"));
        assert!(refused(open.check_host("ll6.test")).contains("link-local"));
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
                ("a.lod.example", &["93.184.216.35"]),
                ("private.lod.example", &["10.9.9.9"]),
                ("lan.lod.example", &["10.1.2.3"]),
                ("other.example", &["93.184.216.36"]),
                ("lod.example", &["93.184.216.37"]),
                ("in-net.test", &["10.1.200.1"]),
                ("half-in-net.test", &["10.1.200.1", "10.2.0.1"]),
            ])
        };
        // named hosts, whatever they resolve to
        assert!(p.check_host("sparql.example.org").is_ok());
        assert!(p.check_host("Local.Test.").is_ok());
        // subdomains, public addresses only (unless covered by a network)
        assert!(p.check_host("a.lod.example").is_ok());
        assert!(refused(p.check_host("private.lod.example")).contains("private address"));
        assert!(p.check_host("lan.lod.example").is_ok());
        assert!(refused(p.check_host("lod.example")).contains("not in the outbound allowlist"));
        // names covered by a network, entirely
        assert!(p.check_host("in-net.test").is_ok());
        assert!(refused(p.check_host("half-in-net.test")).contains("10.2.0.1, a private address"));
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
}
