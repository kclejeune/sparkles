//! Where `serve` may listen without authentication, and which requests an open server
//! answers. Without `--auth-config` every caller is the local principal, which may read,
//! write and administer everything, so an open server stays on loopback (or a Unix
//! socket) unless the operator says otherwise with `--allow-open-network`, and it
//! answers only the `Host` names it is known by ([`Hosts`]: a web page that rebinds its
//! own DNS name to 127.0.0.1 is refused) and no cross-site browser requests
//! (`auth::middleware`).

/// The environment variable of `--allow-open-network`.
pub const ALLOW_OPEN_NETWORK_ENV: &str = "SPARKLES_ALLOW_OPEN_NETWORK";

/// Whether `host` is a loopback address (`localhost`, `127.0.0.0/8`, `::1`).
pub fn loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(any(feature = "auth", test))]
/// Whether `host` is a loopback listener: a loopback address or a Unix socket (`unix`).
pub fn local_listener(host: &str) -> bool {
    host == "unix" || loopback(host)
}

/// A valid browser origin for CORS: `scheme://host[:port]`, no path, not `*`.
pub fn valid_origin(o: &str) -> bool {
    let Some((scheme, rest)) = o.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && !rest.is_empty()
        && !rest.contains(['/', '?', '#', '*', '@', ' '])
}

/// The `Host` names an open server answers: IP literals (a page cannot rebind one),
/// `localhost` and `*.localhost`, the `--host` it listens on, and every `--public-host`.
/// Any other name is what a DNS-rebinding page sends after pointing its own name at
/// the server, so it is refused (`421`).
#[derive(Clone, Debug, Default)]
pub struct Hosts {
    /// lowercase, without port or trailing dot
    names: Vec<String>,
}

impl Hosts {
    /// `bound` is the `--host` value (`unix` for a socket), `public` the `--public-host`
    /// names (`NAME` or `NAME:PORT`; the port is ignored).
    pub fn new(bound: &str, public: &[String]) -> anyhow::Result<Hosts> {
        let mut names = Vec::new();
        for n in public {
            let h = host_name(n);
            if h.is_empty() || n.contains(['/', '@', ' ', '*']) {
                anyhow::bail!(
                    "--public-host '{n}': expected a host name such as sparql.example.org"
                );
            }
            names.push(h);
        }
        if bound != "unix" {
            names.push(host_name(bound));
        }
        Ok(Hosts { names })
    }

    /// Whether a request with this `Host` (or `:authority`) is answered.
    pub fn allows(&self, host: &str) -> bool {
        let h = host_name(host);
        h.parse::<std::net::IpAddr>().is_ok()
            || h == "localhost"
            || h.ends_with(".localhost")
            || self.names.contains(&h)
    }
}

/// The name of a `Host` value: lowercase, without port, brackets or trailing dot.
fn host_name(host: &str) -> String {
    let h = host.trim();
    let h = match h.strip_prefix('[') {
        // [v6] or [v6]:port
        Some(rest) => rest.split(']').next().unwrap_or(""),
        // name:port (a bare IPv6 address has more than one colon)
        None if h.matches(':').count() == 1 => h.split(':').next().unwrap_or(""),
        None => h,
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// Refuse an unauthenticated listener on a network address unless `allow_open`.
pub fn check(host: &str, unix_socket: bool, auth: bool, allow_open: bool) -> anyhow::Result<()> {
    if unix_socket || auth || allow_open || loopback(host) {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to listen on {host} without authentication: any client that can reach \
         the port could read, write and administer every dataset. Use --auth-config FILE, \
         listen on a loopback address (--host 127.0.0.1, the default) or a Unix socket, \
         or pass --allow-open-network (or set {ALLOW_OPEN_NETWORK_ENV}=1) to serve it \
         open anyway"
    )
}

/// Check `--metrics-addr` (`HOST:PORT`): without authentication, a network address
/// needs `allow_open`, since the metrics name every dataset.
pub fn check_metrics_addr(addr: &str, auth: bool, allow_open: bool) -> anyhow::Result<()> {
    let Some((host, port)) = addr.rsplit_once(':') else {
        anyhow::bail!("--metrics-addr '{addr}': expected HOST:PORT, such as 127.0.0.1:9464");
    };
    if host.is_empty() || port.parse::<u16>().is_err() {
        anyhow::bail!("--metrics-addr '{addr}': expected HOST:PORT, such as 127.0.0.1:9464");
    }
    if auth || allow_open || loopback(host) {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to serve metrics on {host} without authentication: they name every \
         dataset and count its requests. Use --auth-config FILE (callers then need the \
         metrics permission), a loopback address, or --allow-open-network"
    )
}

/// Whether a rate-limit configuration limits requests (a `query`, `update` or `admin`
/// limit, on every dataset or on one); `auth` and `preauth` alone limit only logins and
/// authentication failures.
pub fn requests_limited(cfg: Option<&crate::ratelimit::Config>) -> bool {
    use crate::ratelimit::Class;
    let limits = |m: &std::collections::BTreeMap<String, crate::ratelimit::Limit>| {
        [Class::Query, Class::Update, Class::Admin]
            .iter()
            .any(|c| m.get(c.as_str()).is_some_and(|l| !l.is_unlimited()))
    };
    cfg.is_some_and(|c| limits(&c.classes) || c.datasets.values().any(limits))
}

/// The startup warnings of a network listener without authentication or request rate
/// limits (`rate_limited`: [`requests_limited`]).
pub fn warnings(host: &str, unix_socket: bool, auth: bool, rate_limited: bool) -> Vec<String> {
    if unix_socket || loopback(host) {
        return Vec::new();
    }
    let mut w = Vec::new();
    if !auth {
        w.push(format!(
            "SERVING WITHOUT AUTHENTICATION ON {host} (--allow-open-network): every client \
             that can reach this port may read, write and administer every dataset; a \
             reverse proxy that authenticates does not protect a port it can be bypassed on"
        ));
    }
    if !rate_limited {
        w.push(format!(
            "rate limiting is off on {host}: one client can use all query, update and \
             admin capacity (see --rate-limit / --rate-limit-config; with --auth-config \
             only failed authentications are limited by default)"
        ));
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses() {
        for h in [
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
            "localhost",
            "LOCALHOST",
        ] {
            assert!(loopback(h), "{h}");
        }
        for h in [
            "0.0.0.0",
            "::",
            "[::]",
            "10.0.0.1",
            "192.168.1.2",
            "example.org",
            "",
        ] {
            assert!(!loopback(h), "{h}");
        }
    }

    #[test]
    fn metrics_addresses() {
        assert!(check_metrics_addr("127.0.0.1:9464", false, false).is_ok());
        assert!(check_metrics_addr("[::1]:9464", false, false).is_ok());
        assert!(check_metrics_addr("localhost:9464", false, false).is_ok());
        let e = check_metrics_addr("0.0.0.0:9464", false, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--allow-open-network"), "{e}");
        assert!(check_metrics_addr("0.0.0.0:9464", true, false).is_ok());
        assert!(check_metrics_addr("0.0.0.0:9464", false, true).is_ok());
        for bad in ["9464", ":9464", "127.0.0.1:", "127.0.0.1:x"] {
            assert!(check_metrics_addr(bad, true, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn open_network_listeners_need_auth_or_consent() {
        let e = check("0.0.0.0", false, false, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--allow-open-network"), "{e}");
        assert!(e.contains(ALLOW_OPEN_NETWORK_ENV), "{e}");
        assert!(e.contains("--auth-config"), "{e}");
        assert!(check("0.0.0.0", false, true, false).is_ok());
        assert!(check("0.0.0.0", false, false, true).is_ok());
        assert!(check("0.0.0.0", true, false, false).is_ok());
        assert!(check("127.0.0.1", false, false, false).is_ok());
    }

    #[test]
    fn warnings_on_network_listeners_only() {
        assert!(warnings("127.0.0.1", false, false, false).is_empty());
        assert!(warnings("0.0.0.0", true, false, false).is_empty());
        let w = warnings("0.0.0.0", false, false, false);
        assert_eq!(w.len(), 2);
        assert!(w[0].contains("WITHOUT AUTHENTICATION"));
        assert!(w[1].contains("rate limiting is off"));
        let w = warnings("::", false, true, false);
        assert_eq!(w.len(), 1);
        assert!(warnings("::", false, true, true).is_empty());
    }

    #[test]
    fn host_names_of_an_open_server() {
        let h = Hosts::new("127.0.0.1", &["sparql.example.org".into()]).unwrap();
        for ok in [
            "127.0.0.1:3030",
            "127.0.0.1",
            "[::1]:3030",
            "[::1]",
            "::1",
            "localhost:3030",
            "LOCALHOST",
            "localhost.",
            "app.localhost:5173",
            // an IP literal cannot be rebound
            "192.168.1.20:3030",
            "sparql.example.org",
            "Sparql.Example.Org:443",
        ] {
            assert!(h.allows(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "evil.example:3030",
            "localhost.evil.example",
            "127.0.0.1.nip.io",
            "sparql.example.org.evil.example",
            "",
        ] {
            assert!(!h.allows(bad), "{bad}");
        }
        // the --host name, and never `unix`
        assert!(
            Hosts::new("myhost.lan", &[])
                .unwrap()
                .allows("myhost.lan:3030")
        );
        assert!(!Hosts::new("unix", &[]).unwrap().allows("unix"));
        for bad in ["https://sparql.example.org", "a/b", "*.example.org", ""] {
            assert!(Hosts::new("127.0.0.1", &[bad.into()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn origins_and_local_listeners() {
        assert!(valid_origin("https://yasgui.example"));
        assert!(valid_origin("http://localhost:5173"));
        for bad in [
            "*",
            "https://a.example/path",
            "yasgui.example",
            "ftp://a",
            "https://",
        ] {
            assert!(!valid_origin(bad), "{bad}");
        }
        for h in [
            "unix",
            "127.0.0.1",
            "127.0.1.1",
            "::1",
            "[::1]",
            "localhost",
        ] {
            assert!(local_listener(h), "{h}");
        }
        assert!(!local_listener("0.0.0.0"));
    }

    #[test]
    fn request_limits_beyond_authentication() {
        use crate::ratelimit::Config;
        let cfg = |flags: &[&str]| {
            let mut c = Config::default();
            for f in flags {
                c.apply_flag(f).unwrap();
            }
            c
        };
        assert!(!requests_limited(None));
        // the default pre-authentication limit of an auth server limits no requests
        assert!(!requests_limited(Some(&cfg(&["preauth=30/min,burst=60"]))));
        assert!(!requests_limited(Some(&cfg(&["auth=10/min"]))));
        assert!(!requests_limited(Some(&cfg(&["query=off"]))));
        assert!(requests_limited(Some(&cfg(&["query=100/s"]))));
        assert!(requests_limited(Some(&cfg(&["update=concurrency=2"]))));
        assert!(requests_limited(Some(&cfg(&["admin@ds=1/s"]))));
    }
}
