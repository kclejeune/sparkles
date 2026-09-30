//! The connection peer, and trusted-header authentication behind a forward-auth proxy
//! (oauth2-proxy, Authelia, `tailscale serve`, Cloudflare Access).

#[cfg(feature = "auth")]
use super::config::{ProxyCfg, proxy_preset};
#[cfg(feature = "auth")]
use axum::http::HeaderMap;
use std::net::{IpAddr, SocketAddr};

/// Where a connection came from: a TCP address, or the Unix socket (`--unix-socket`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    Tcp(SocketAddr),
    Unix,
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Peer
{
    fn connect_info(s: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Peer {
        Peer::Tcp(*s.remote_addr())
    }
}

#[cfg(unix)]
impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for Peer
{
    fn connect_info(_: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Peer {
        Peer::Unix
    }
}

/// The peers whose identity headers are honored.
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies {
    pub nets: Vec<ipnet::IpNet>,
    pub unix: bool,
}

impl TrustedProxies {
    pub fn parse(entries: &[String]) -> TrustedProxies {
        let mut t = TrustedProxies::default();
        for e in entries {
            if e == "unix" {
                t.unix = true;
            } else if let Ok(n) = e.parse::<ipnet::IpNet>() {
                t.nets.push(n);
            } else if let Ok(a) = e.parse::<IpAddr>() {
                t.nets.push(ipnet::IpNet::from(a));
            }
        }
        t
    }

    pub fn trusts(&self, peer: Option<&Peer>) -> bool {
        match peer {
            Some(Peer::Unix) => self.unix,
            Some(Peer::Tcp(a)) => {
                let ip = a.ip().to_canonical();
                self.nets.iter().any(|n| n.contains(&ip))
            }
            None => false,
        }
    }
}

/// Header settings of `[proxy]`, resolved from its preset and overrides.
#[cfg(feature = "auth")]
#[derive(Clone, Debug)]
pub struct ProxySettings {
    pub trusted: TrustedProxies,
    pub user_header: String,
    pub email_header: Option<String>,
    pub groups_header: Option<String>,
    pub separator: String,
    pub name_from_email: bool,
    pub logout_url: Option<String>,
}

#[cfg(feature = "auth")]
impl ProxySettings {
    pub fn from_config(c: &ProxyCfg) -> ProxySettings {
        let preset = c.preset.as_deref().and_then(proxy_preset);
        ProxySettings {
            trusted: TrustedProxies::parse(&c.trusted),
            user_header: c
                .user_header
                .clone()
                .or(preset.map(|p| p.0.to_string()))
                .unwrap_or_default(),
            email_header: c.email_header.clone().or(preset.map(|p| p.1.to_string())),
            groups_header: c
                .groups_header
                .clone()
                .or(preset.and_then(|p| p.2).map(str::to_string)),
            separator: c.groups_separator.clone(),
            name_from_email: c.name_from == "email",
            logout_url: c.logout_url.clone(),
        }
    }

    /// Whether the request carries any identity header of this proxy.
    pub fn has_headers(&self, h: &HeaderMap) -> bool {
        [
            Some(&self.user_header),
            self.email_header.as_ref(),
            self.groups_header.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|n| h.contains_key(n.as_str()))
    }

    /// The name and groups the proxy asserts (`None`: no usable user header).
    pub fn identity(&self, h: &HeaderMap) -> Option<(String, Vec<String>)> {
        let header = if self.name_from_email {
            self.email_header.as_ref().unwrap_or(&self.user_header)
        } else {
            &self.user_header
        };
        let name = h.get(header.as_str())?.to_str().ok()?.trim().to_string();
        // 1–256 bytes of visible ASCII
        if name.is_empty() || name.len() > 256 || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        let groups = self
            .groups_header
            .as_ref()
            .and_then(|g| h.get(g.as_str()))
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(self.separator.as_str())
                    .map(str::trim)
                    .filter(|g| !g.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some((name, groups))
    }
}

#[cfg(all(test, feature = "auth"))]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn trust() {
        let t = TrustedProxies::parse(&["127.0.0.1/32".into(), "::1".into(), "unix".into()]);
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let other: SocketAddr = "127.0.0.2:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5000".parse().unwrap();
        assert!(t.trusts(Some(&Peer::Tcp(local))));
        assert!(t.trusts(Some(&Peer::Tcp(mapped))));
        assert!(!t.trusts(Some(&Peer::Tcp(other))));
        assert!(t.trusts(Some(&Peer::Unix)));
        assert!(!t.trusts(None));
    }

    #[test]
    fn presets_and_identity() {
        let cfg = ProxyCfg {
            preset: Some("authelia".into()),
            trusted: vec!["127.0.0.1/32".into()],
            user_header: None,
            email_header: None,
            groups_header: None,
            groups_separator: ",".into(),
            name_from: "user".into(),
            logout_url: None,
        };
        let s = ProxySettings::from_config(&cfg);
        assert_eq!(s.user_header, "Remote-User");
        let mut h = HeaderMap::new();
        assert!(!s.has_headers(&h));
        h.insert("remote-user", HeaderValue::from_static("dave"));
        h.insert(
            "remote-groups",
            HeaderValue::from_static("sparkles, kg-editors,"),
        );
        assert!(s.has_headers(&h));
        assert_eq!(
            s.identity(&h),
            Some(("dave".into(), vec!["sparkles".into(), "kg-editors".into()]))
        );
        h.insert("remote-user", HeaderValue::from_static("has space"));
        assert_eq!(s.identity(&h), None);
    }
}
