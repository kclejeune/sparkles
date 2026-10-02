//! Native TLS for `serve --tls-cert FILE --tls-key FILE` (rustls with the aws-lc-rs
//! provider; TLS 1.2 and 1.3; HTTP/2 and HTTP/1.1 negotiated through ALPN).
//!
//! Most deployments terminate TLS at a reverse proxy; this is for those that do not.
//! The certificate chain and key are PEM files. They are read again on SIGHUP and when
//! either file's modification time changes (checked every [`WATCH_INTERVAL`]), so a
//! renewal by an ACME client needs no restart; a pair that does not load (a missing
//! file, a key that does not match the certificate) keeps the one in use.
//!
//! The accept loop never waits for a handshake: each runs in a task of its own, at most
//! [`MAX_HANDSHAKES`] at once and each for at most [`HANDSHAKE_TIMEOUT`], and finished
//! connections reach the server through a channel.

use anyhow::{Context, Result, bail};
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert, ServerConfig};
use rustls::sign::CertifiedKey;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// A handshake that has not finished after this long is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshakes in progress at most; further connections wait to be accepted.
pub const MAX_HANDSHAKES: usize = 1024;
/// How often the certificate and key files are checked for changes.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(60);
/// Finished handshakes waiting for the server to take them.
const QUEUE: usize = 256;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Read a PEM certificate chain and private key, and check that they belong together.
pub fn load_pair(cert: &Path, key: &Path) -> Result<CertifiedKey> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading the TLS certificate {}", cert.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("{}: invalid PEM", cert.display()))?;
    if chain.is_empty() {
        bail!("{}: no certificate in the file", cert.display());
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("reading the TLS private key {}", key.display()))?;
    CertifiedKey::from_der(chain, key, &provider()).map_err(|e| {
        anyhow::anyhow!(
            "the TLS key does not fit the certificate {}: {e}",
            cert.display()
        )
    })
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// The certificate the server presents, swapped whole when the files change.
#[derive(Debug)]
pub struct Certs {
    cert: PathBuf,
    key: PathBuf,
    current: ArcSwap<CertifiedKey>,
    /// modification times of the files the current pair was read from
    stamp: Mutex<(Option<SystemTime>, Option<SystemTime>)>,
}

impl Certs {
    /// Load the pair (an error stops the server before it binds).
    pub fn open(cert: &Path, key: &Path) -> Result<Arc<Certs>> {
        let stamp = (mtime(cert), mtime(key));
        let pair = load_pair(cert, key)?;
        Ok(Arc::new(Certs {
            cert: cert.to_path_buf(),
            key: key.to_path_buf(),
            current: ArcSwap::from_pointee(pair),
            stamp: Mutex::new(stamp),
        }))
    }

    /// Read the files again; on error the pair in use stays.
    pub fn reload(&self) -> Result<()> {
        let stamp = (mtime(&self.cert), mtime(&self.key));
        let pair = load_pair(&self.cert, &self.key)?;
        self.current.store(Arc::new(pair));
        *self.stamp.lock() = stamp;
        Ok(())
    }

    /// Reload when either file's modification time changed; `None` when neither did.
    pub fn reload_if_changed(&self) -> Option<Result<()>> {
        let now = (mtime(&self.cert), mtime(&self.key));
        if *self.stamp.lock() == now {
            return None;
        }
        let r = self.reload();
        if r.is_err() {
            // a pair caught halfway through a renewal is retried at the next check, but a
            // broken one is not reported every minute
            *self.stamp.lock() = now;
        }
        Some(r)
    }

    /// The end-entity certificate in use (tests).
    #[cfg(test)]
    pub fn leaf(&self) -> CertificateDer<'static> {
        self.current.load().cert[0].clone()
    }
}

impl ResolvesServerCert for Certs {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.load_full())
    }
}

/// The rustls configuration: the certificates of `certs`, ALPN `h2` then `http/1.1`.
pub fn server_config(certs: Arc<Certs>) -> Result<Arc<ServerConfig>> {
    let mut c = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?
        .with_no_client_auth()
        .with_cert_resolver(certs);
    c.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(c))
}

/// A TCP listener whose connections are TLS sessions. It implements axum's `Listener`,
/// so `axum::serve` runs over it unchanged.
pub struct TlsListener {
    rx: tokio::sync::mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    /// Accept on `tcp` and run handshakes in the background. The background task ends
    /// when the listener is dropped (the server stopped accepting).
    pub fn new(tcp: TcpListener, config: Arc<ServerConfig>) -> Result<TlsListener> {
        let local = tcp.local_addr()?;
        let (tx, rx) = tokio::sync::mpsc::channel(QUEUE);
        let acceptor = TlsAcceptor::from(config);
        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES));
        tokio::spawn(async move {
            let mut tcp = tcp;
            loop {
                let Ok(permit) = permits.clone().acquire_owned().await else {
                    return;
                };
                let (stream, addr) = tokio::select! {
                    // axum's accept: connection errors are skipped, others wait a second
                    a = axum::serve::Listener::accept(&mut tcp) => a,
                    () = tx.closed() => return,
                };
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, addr)).await;
                        }
                        Ok(Err(e)) => tracing::debug!("TLS handshake with {addr} failed: {e}"),
                        Err(_) => tracing::debug!("TLS handshake with {addr} timed out"),
                    }
                });
            }
        });
        Ok(TlsListener { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(c) => c,
            // the accept task ended (it cannot while this receiver lives): accept nothing
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local)
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for crate::auth::Peer
{
    fn connect_info(s: axum::serve::IncomingStream<'_, TlsListener>) -> crate::auth::Peer {
        crate::auth::Peer::Tcp(*s.remote_addr())
    }
}

/// Requests that arrived over TLS say so to the code that derives the scheme from
/// `X-Forwarded-Proto` (cookie attributes, the server's own origin, external URLs):
/// the header is set to `https` unless a proxy in front already set it.
pub async fn mark_https(mut req: axum::extract::Request) -> axum::extract::Request {
    if !req.headers().contains_key("x-forwarded-proto") {
        req.headers_mut().insert(
            "x-forwarded-proto",
            axum::http::HeaderValue::from_static("https"),
        );
    }
    req
}

/// Reload the certificate and key on SIGHUP, and when their files change.
pub fn spawn_reload(certs: Arc<Certs>) {
    let watched = certs.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(WATCH_INTERVAL);
        t.tick().await;
        loop {
            t.tick().await;
            match watched.reload_if_changed() {
                Some(Ok(())) => tracing::info!("TLS certificate reloaded (the files changed)"),
                Some(Err(e)) => tracing::error!("TLS certificate not reloaded: {e:#}"),
                None => {}
            }
        }
    });
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut hup) = signal(SignalKind::hangup()) else {
            tracing::warn!("cannot listen for SIGHUP: TLS certificate reload on signal disabled");
            return;
        };
        while hup.recv().await.is_some() {
            match certs.reload() {
                Ok(()) => tracing::info!("TLS certificate reloaded"),
                Err(e) => tracing::error!("TLS certificate not reloaded: {e:#}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const CA: &str = include_str!("tls/testdata/ca.pem");
    const CERT_A: &str = include_str!("tls/testdata/cert-a.pem");
    const KEY_A: &str = include_str!("tls/testdata/key-a.pem");
    const CERT_B: &str = include_str!("tls/testdata/cert-b.pem");
    const KEY_B: &str = include_str!("tls/testdata/key-b.pem");

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn der(pem: &str) -> CertificateDer<'static> {
        CertificateDer::from_pem_slice(pem.as_bytes()).unwrap()
    }

    #[test]
    fn pairs_must_match() {
        let d = tempfile::tempdir().unwrap();
        let (ca, ka) = (
            write(d.path(), "a.pem", CERT_A),
            write(d.path(), "a.key", KEY_A),
        );
        let kb = write(d.path(), "b.key", KEY_B);
        load_pair(&ca, &ka).unwrap();
        let e = load_pair(&ca, &kb).unwrap_err().to_string();
        assert!(e.contains("does not fit"), "{e}");
        let empty = write(d.path(), "empty.pem", "");
        assert!(load_pair(&empty, &ka).is_err());
        assert!(load_pair(&d.path().join("missing.pem"), &ka).is_err());
        // a certificate where the key belongs
        assert!(load_pair(&ca, &ca).is_err());
    }

    #[test]
    fn reload_keeps_the_old_pair_on_error() {
        let d = tempfile::tempdir().unwrap();
        let cert = write(d.path(), "c.pem", CERT_A);
        let key = write(d.path(), "k.pem", KEY_A);
        let certs = Certs::open(&cert, &key).unwrap();
        assert_eq!(certs.leaf(), der(CERT_A));
        assert!(certs.reload_if_changed().is_none());
        // a renewal caught halfway: the new certificate with the old key
        std::fs::write(&cert, CERT_B).unwrap();
        assert!(certs.reload().is_err());
        assert_eq!(certs.leaf(), der(CERT_A));
        std::fs::write(&key, KEY_B).unwrap();
        certs.reload().unwrap();
        assert_eq!(certs.leaf(), der(CERT_B));
    }

    /// A server over TLS on 127.0.0.1 answering `/` with the HTTP version it saw.
    async fn serve(certs: Arc<Certs>) -> SocketAddr {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let l = TlsListener::new(tcp, server_config(certs).unwrap()).unwrap();
        let addr = l.local;
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::get(
                    |axum::extract::ConnectInfo(p): axum::extract::ConnectInfo<
                        crate::auth::Peer,
                    >,
                     req: axum::extract::Request| async move {
                        format!(
                            "{:?} {} {}",
                            req.version(),
                            matches!(p, crate::auth::Peer::Tcp(_)),
                            req.headers()
                                .get("x-forwarded-proto")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("-")
                        )
                    },
                ),
            )
            .layer(axum::middleware::map_request(mark_https));
        tokio::spawn(async move {
            axum::serve(
                l,
                app.into_make_service_with_connect_info::<crate::auth::Peer>(),
            )
            .await
            .unwrap()
        });
        addr
    }

    fn client(h2: bool) -> reqwest::Client {
        let b = reqwest::Client::builder()
            .tls_certs_only([reqwest::Certificate::from_pem(CA.as_bytes()).unwrap()])
            .tls_info(true);
        if h2 {
            b.http2_prior_knowledge()
        } else {
            b.http1_only()
        }
        .build()
        .unwrap()
    }

    fn peer_cert(r: &reqwest::Response) -> Vec<u8> {
        r.extensions()
            .get::<reqwest::tls::TlsInfo>()
            .and_then(|i| i.peer_certificate())
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn serves_http1_and_http2_and_reloads() {
        let d = tempfile::tempdir().unwrap();
        let cert = write(d.path(), "c.pem", CERT_A);
        let key = write(d.path(), "k.pem", KEY_A);
        let certs = Certs::open(&cert, &key).unwrap();
        let addr = serve(certs.clone()).await;
        let url = format!("https://localhost:{}/", addr.port());
        let r = client(false).get(&url).send().await.unwrap();
        assert_eq!(r.version(), reqwest::Version::HTTP_11);
        assert_eq!(peer_cert(&r), der(CERT_A).to_vec());
        assert_eq!(r.text().await.unwrap(), "HTTP/1.1 true https");
        let r = client(true).get(&url).send().await.unwrap();
        assert_eq!(r.version(), reqwest::Version::HTTP_2);
        assert_eq!(r.text().await.unwrap(), "HTTP/2.0 true https");
        // a renewal: new connections get the new certificate
        std::fs::write(&cert, CERT_B).unwrap();
        std::fs::write(&key, KEY_B).unwrap();
        certs.reload().unwrap();
        let r = client(false).get(&url).send().await.unwrap();
        assert_eq!(peer_cert(&r), der(CERT_B).to_vec());
        // plain HTTP on the TLS port gets no answer
        let plain = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/", addr.port()))
            .send()
            .await;
        assert!(plain.is_err());
    }

    #[tokio::test]
    async fn a_stalled_handshake_does_not_block_others() {
        let d = tempfile::tempdir().unwrap();
        let certs = Certs::open(
            &write(d.path(), "c.pem", CERT_A),
            &write(d.path(), "k.pem", KEY_A),
        )
        .unwrap();
        let addr = serve(certs).await;
        // connections that never send a ClientHello
        let mut idle = Vec::new();
        for _ in 0..8 {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            client(false)
                .get(format!("https://localhost:{}/", addr.port()))
                .send(),
        )
        .await
        .expect("a handshake waited for idle connections")
        .unwrap();
        assert!(r.status().is_success());
    }
}
