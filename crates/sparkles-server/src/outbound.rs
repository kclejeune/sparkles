//! `--outbound-*` flags: where SERVICE and `LOAD <http…>` may connect.
//!
//! `serve` and `mcp` refuse loopback and private destinations unless
//! `--outbound-allow-private`; the local `query` and `update`, run by the operator on
//! their own machine, allow them unless `--outbound-block-private`. Link-local
//! addresses (the cloud metadata service) need `--outbound-allow` either way.

use anyhow::{Result, bail};
use sparkles::outbound::{Allow, OutboundPolicy};
use std::time::Duration;

#[derive(clap::Args, Clone, Debug)]
pub struct OutboundArgs {
    /// Let SERVICE and LOAD reach loopback, private (RFC 1918), shared (100.64.0.0/10)
    /// and unique-local (fc00::/7) addresses (the default of the local query and
    /// update); link-local ones (169.254.169.254) need --outbound-allow
    #[arg(long, conflicts_with = "outbound_block_private")]
    pub outbound_allow_private: bool,
    /// Refuse loopback, private, shared and unique-local destinations (the default of
    /// serve and mcp)
    #[arg(long)]
    pub outbound_block_private: bool,
    /// Contact only these destinations (repeatable): a host name (any address),
    /// *.domain (public addresses), or an address or CIDR network (any address)
    #[arg(long, value_name = "HOST_OR_CIDR")]
    pub outbound_allow: Vec<String>,
    /// Total time of one SERVICE or LOAD request, in seconds
    #[arg(long, value_name = "SECS", default_value_t = sparkles::outbound::DEFAULT_TIMEOUT.as_secs_f64())]
    pub outbound_timeout: f64,
    /// Largest SERVICE or LOAD response, in MiB (decompressed)
    #[arg(long, value_name = "N", default_value_t = sparkles::outbound::DEFAULT_MAX_RESPONSE_BYTES >> 20)]
    pub outbound_max_mb: u64,
}

impl OutboundArgs {
    /// The policy of a server (`serve`, `mcp`): private destinations are refused
    /// unless `--outbound-allow-private`.
    pub fn policy(&self) -> Result<OutboundPolicy> {
        self.policy_with(!sparkles::outbound::BLOCK_PRIVATE_BY_DEFAULT)
    }

    /// The policy of the local `query` and `update`: private destinations are allowed
    /// unless `--outbound-block-private`.
    pub fn local_policy(&self) -> Result<OutboundPolicy> {
        self.policy_with(true)
    }

    fn policy_with(&self, allow_private: bool) -> Result<OutboundPolicy> {
        if !(self.outbound_timeout.is_finite() && self.outbound_timeout > 0.0) {
            bail!("--outbound-timeout must be a positive number of seconds");
        }
        if self.outbound_max_mb == 0 {
            bail!("--outbound-max-mb must be at least 1");
        }
        let allow = self
            .outbound_allow
            .iter()
            .map(|a| {
                a.parse::<Allow>()
                    .map_err(|e| anyhow::anyhow!("--outbound-allow: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let timeout = Duration::from_secs_f64(self.outbound_timeout);
        Ok(OutboundPolicy {
            allow_private: (allow_private || self.outbound_allow_private)
                && !self.outbound_block_private,
            allow,
            timeout,
            connect_timeout: timeout.min(sparkles::outbound::DEFAULT_CONNECT_TIMEOUT),
            max_response_bytes: self.outbound_max_mb.saturating_mul(1 << 20),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        outbound: OutboundArgs,
    }

    fn policy(args: &[&str]) -> Result<OutboundPolicy> {
        let cli = Cli::try_parse_from(std::iter::once("sparkles").chain(args.iter().copied()))?;
        cli.outbound.policy()
    }

    #[test]
    fn flags() {
        let d = policy(&[]).unwrap();
        assert!(!d.allow_private);
        assert!(d.allow.is_empty());
        assert_eq!(d.timeout, sparkles::outbound::DEFAULT_TIMEOUT);
        assert_eq!(
            d.max_response_bytes,
            sparkles::outbound::DEFAULT_MAX_RESPONSE_BYTES
        );
        let p = policy(&[
            "--outbound-allow-private",
            "--outbound-allow",
            "localhost",
            "--outbound-allow",
            "10.0.0.0/8",
            "--outbound-timeout",
            "2.5",
            "--outbound-max-mb",
            "16",
        ])
        .unwrap();
        assert!(p.allow_private);
        assert_eq!(p.allow.len(), 2);
        assert_eq!(p.timeout, Duration::from_millis(2500));
        assert_eq!(p.connect_timeout, Duration::from_millis(2500));
        assert_eq!(p.max_response_bytes, 16 << 20);
        assert!(p.check_url("http://10.1.2.3:3030/ds/sparql").is_ok());
        assert!(policy(&["--outbound-allow", "http://x/"]).is_err());
        assert!(!policy(&["--outbound-block-private"]).unwrap().allow_private);
        assert!(policy(&["--outbound-allow-private", "--outbound-block-private"]).is_err());
        assert!(policy(&["--outbound-timeout", "0"]).is_err());
        assert!(policy(&["--outbound-max-mb", "0"]).is_err());
    }

    fn local_policy(args: &[&str]) -> Result<OutboundPolicy> {
        let cli = Cli::try_parse_from(std::iter::once("sparkles").chain(args.iter().copied()))?;
        cli.outbound.local_policy()
    }

    #[test]
    fn local_commands_allow_private_destinations() {
        let p = local_policy(&[]).unwrap();
        assert!(p.allow_private);
        for url in [
            "http://127.0.0.1:3030/ds/sparql",
            "http://10.1.2.3/",
            "http://100.64.0.1/",
            "http://[fd00::1]/",
        ] {
            assert!(p.check_url(url).is_ok(), "{url}");
        }
        // link-local addresses (the cloud metadata service) stay refused
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://[fe80::1]/",
        ] {
            assert!(p.check_url(url).is_err(), "{url}");
        }
        assert!(
            local_policy(&["--outbound-allow-private"])
                .unwrap()
                .allow_private
        );
        let strict = local_policy(&["--outbound-block-private"]).unwrap();
        assert!(!strict.allow_private);
        assert!(strict.check_url("http://127.0.0.1/").is_err());
        // an allowlist entry still opens a destination
        let p = local_policy(&["--outbound-allow", "169.254.169.254"]).unwrap();
        assert!(p.check_url("http://169.254.169.254/").is_ok());
    }

    /// The server's queries and updates go through its policy: loopback is refused by
    /// default (403, never contacted) and reached once allowed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_requests_follow_the_policy() {
        use crate::state::{AppState, DbType};
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use std::io::{Read, Write};
        use std::net::SocketAddr;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tower::ServiceExt;

        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let n = conns.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                n.fetch_add(1, Ordering::SeqCst);
                let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = c.read(&mut [0u8; 4096]);
                let body = "<urn:a> <urn:p> <urn:b> .";
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/turtle\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let call = |app: axum::Router, update: bool| async move {
            let text = if update {
                format!("LOAD <http://127.0.0.1:{port}/d.ttl>")
            } else {
                format!("SELECT * {{ SERVICE SILENT <http://127.0.0.1:{port}/x> {{ ?s ?p ?o }} }}")
            };
            let (path, key) = if update {
                ("/ds/update", "update")
            } else {
                ("/ds/sparql", "query")
            };
            let body = format!(
                "{key}={}",
                percent_encoding::utf8_percent_encode(&text, percent_encoding::NON_ALPHANUMERIC)
            );
            let mut req = Request::post(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap();
            let addr: SocketAddr = "127.0.0.1:5555".parse().unwrap();
            req.extensions_mut()
                .insert(axum::extract::ConnectInfo(addr));
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let b = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&b).into_owned())
        };
        let app = |allow_private: bool| {
            let dir = tempfile::tempdir().unwrap();
            let mut st = AppState::new(
                dir.path(),
                sparkles::store::StoreOptions::default(),
                Duration::from_secs(30),
            )
            .unwrap();
            if allow_private {
                st.outbound = policy(&["--outbound-allow-private"]).unwrap();
            }
            let st = Arc::new(st);
            st.attach("ds", DbType::Mem, None).unwrap();
            st.set_phase(crate::obs::Phase::Ready);
            (dir, crate::http::router(st))
        };
        let (_d, closed) = app(false);
        for update in [false, true] {
            let (status, body) = call(closed.clone(), update).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(body.contains("127.0.0.1 is a loopback address"), "{body}");
        }
        assert_eq!(conns.load(Ordering::SeqCst), 0);
        let (_d, open) = app(true);
        for update in [false, true] {
            let (status, body) = call(open.clone(), update).await;
            assert!(status.is_success(), "{status}: {body}");
        }
        assert_eq!(conns.load(Ordering::SeqCst), 2);
    }
}
