//! `--outbound-*` flags: where SERVICE and `LOAD <http…>` may connect.
//!
//! `serve` and `mcp` refuse loopback and private destinations unless
//! `--outbound-allow-private`; the local `query` and `update`, run by the operator on
//! their own machine, allow them unless `--outbound-block-private`. Link-local
//! addresses (the cloud metadata service) need an address or network in
//! `--outbound-allow` either way.
//!
//! Also `serve --load-dir`: which files `LOAD <file:…>` may read.

use anyhow::{Context, Result, bail};
use sparkles::outbound::{Allow, OutboundPolicy};
use sparkles::sparql::FileLoads;
use std::path::Path;
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
    /// Contact only these destinations (repeatable): a host name or *.domain (at public
    /// addresses), or an address or CIDR network (any address in it, private and
    /// link-local ones included)
    #[arg(long, value_name = "HOST_OR_CIDR")]
    pub outbound_allow: Vec<String>,
    /// Total time of one SERVICE or LOAD request, in seconds
    #[arg(long, value_name = "SECS", default_value_t = sparkles::outbound::DEFAULT_TIMEOUT.as_secs_f64())]
    pub outbound_timeout: f64,
    /// Largest SERVICE or LOAD response, in MiB (decompressed)
    #[arg(long, value_name = "N", default_value_t = sparkles::outbound::DEFAULT_MAX_RESPONSE_BYTES >> 20)]
    pub outbound_max_mb: u64,
    /// Bytes all the SERVICE calls and LOADs of one query or update may receive, in MiB
    /// [default: 4 × --outbound-max-mb]
    #[arg(long, value_name = "N")]
    pub outbound_request_max_mb: Option<u64>,
    /// Time all the SERVICE calls and LOADs of one query or update may take, summed, in
    /// seconds [default: 4 × --outbound-timeout]
    #[arg(long, value_name = "SECS")]
    pub outbound_request_timeout: Option<f64>,
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
        let request_timeout = self
            .outbound_request_timeout
            .unwrap_or(4.0 * self.outbound_timeout);
        if !(request_timeout.is_finite() && request_timeout > 0.0) {
            bail!("--outbound-request-timeout must be a positive number of seconds");
        }
        let request_mb = self
            .outbound_request_max_mb
            .unwrap_or(self.outbound_max_mb.saturating_mul(4));
        if request_mb == 0 {
            bail!("--outbound-request-max-mb must be at least 1");
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
            max_request_bytes: request_mb.saturating_mul(1 << 20),
            request_timeout: Duration::from_secs_f64(request_timeout),
            ..Default::default()
        })
    }
}

/// `serve --load-dir DIR`: `LOAD <file:…>` reads files under `DIR` only, and none
/// without the flag. The directory must not hold the data directory (the databases and
/// the auth state).
pub fn file_loads(dir: Option<&Path>, data: &Path) -> Result<FileLoads> {
    let Some(dir) = dir else {
        return Ok(FileLoads::Disabled);
    };
    let loads = FileLoads::under(dir).with_context(|| format!("--load-dir {}", dir.display()))?;
    if let (FileLoads::Under(d), Ok(data)) = (&loads, std::fs::canonicalize(data))
        && data.starts_with(d)
    {
        bail!(
            "--load-dir {} holds the data directory {}; name a directory of its own",
            dir.display(),
            data.display()
        );
    }
    Ok(loads)
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

    /// The totals of one SPARQL request follow the per-request flags unless set.
    #[test]
    fn request_budget_flags() {
        let d = policy(&[]).unwrap();
        assert_eq!(
            d.max_request_bytes,
            sparkles::outbound::DEFAULT_MAX_REQUEST_BYTES
        );
        assert_eq!(
            d.request_timeout,
            sparkles::outbound::DEFAULT_REQUEST_TIMEOUT
        );
        let p = policy(&["--outbound-max-mb", "16", "--outbound-timeout", "5"]).unwrap();
        assert_eq!(p.max_request_bytes, 64 << 20);
        assert_eq!(p.request_timeout, Duration::from_secs(20));
        let p = policy(&[
            "--outbound-request-max-mb",
            "1",
            "--outbound-request-timeout",
            "0.5",
        ])
        .unwrap();
        assert_eq!(p.max_request_bytes, 1 << 20);
        assert_eq!(p.request_timeout, Duration::from_millis(500));
        assert!(policy(&["--outbound-request-max-mb", "0"]).is_err());
        assert!(policy(&["--outbound-request-timeout", "0"]).is_err());
    }

    #[test]
    fn load_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let files = tmp.path().join("files");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&files).unwrap();
        assert_eq!(file_loads(None, &data).unwrap(), FileLoads::Disabled);
        assert_eq!(
            file_loads(Some(&files), &data).unwrap(),
            FileLoads::Under(std::fs::canonicalize(&files).unwrap())
        );
        // not the data directory, nor a directory holding it
        assert!(file_loads(Some(&data), &data).is_err());
        assert!(file_loads(Some(tmp.path()), &data).is_err());
        assert!(file_loads(Some(&tmp.path().join("missing")), &data).is_err());
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

    /// The LOADs of one update share `--outbound-request-max-mb`: past it the update
    /// answers 507 naming the budget, and commits nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loads_of_one_update_share_the_request_budget() {
        use crate::state::{AppState, DbType};
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use std::io::{Read, Write};
        use std::sync::Arc;
        use tower::ServiceExt;

        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = c.read(&mut [0u8; 4096]);
                let body: String = (0..12_000)
                    .map(|i| format!("<urn:s{i}> <urn:p> \"{i:0>20}\" .\n"))
                    .collect();
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\nconnection: close\r\n\r\n{body}"
                );
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(
            dir.path(),
            sparkles::store::StoreOptions::default(),
            Duration::from_secs(30),
        )
        .unwrap();
        st.outbound =
            policy(&["--outbound-allow-private", "--outbound-request-max-mb", "1"]).unwrap();
        let st = Arc::new(st);
        st.attach("ds", DbType::Mem, None).unwrap();
        st.set_phase(crate::obs::Phase::Ready);
        let app = crate::http::router(st.clone());
        let update = format!(
            "LOAD <http://127.0.0.1:{port}/a.nt> INTO GRAPH <urn:a> ; \
             LOAD <http://127.0.0.1:{port}/b.nt> INTO GRAPH <urn:b>"
        );
        let mut req = Request::post("/ds/update")
            .header("content-type", "application/sparql-update")
            .body(Body::from(update))
            .unwrap();
        let addr: std::net::SocketAddr = "127.0.0.1:5555".parse().unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(addr));
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::INSUFFICIENT_STORAGE);
        let b = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(j["budget"], "outbound-bytes", "{j}");
        assert_eq!(j["limit"], 1 << 20);
        assert_eq!(st.get("ds").unwrap().store.snapshot().len(), 0);
    }
}
