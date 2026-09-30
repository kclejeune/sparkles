//! Where `serve` may listen without authentication. Without `--auth-config` every
//! caller is the local principal, which may read, write and administer everything, so
//! an open server stays on loopback (or a Unix socket) unless the operator says
//! otherwise with `--allow-open-network`.

/// The environment variable of `--allow-open-network`.
pub const ALLOW_OPEN_NETWORK_ENV: &str = "SPARKLES_ALLOW_OPEN_NETWORK";

/// Whether `host` is a loopback address (`localhost`, `127.0.0.0/8`, `::1`).
pub fn loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
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

/// The startup warnings of a network listener without authentication or rate limits.
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
             authentication capacity (see --rate-limit / --rate-limit-config)"
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
}
