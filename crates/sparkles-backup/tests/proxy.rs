//! Environment proxies (`HTTP_PROXY`, `HTTPS_PROXY`) and the outbound policy: an S3
//! repository under a policy (registered through a server's API) connects to its
//! endpoint directly, never through the environment's proxy, which would reach the
//! endpoint past the policy's address checks; one without a policy (a config-file
//! repository, the CLI) keeps the environment's proxy. Its own test binary: it sets
//! process environment variables.

#![cfg(feature = "s3")]

use object_store::ObjectStoreExt;
use sparkles_backup::repo::build_store_with;
use sparkles_backup::{Credentials, RepoConfig, RepoType};
use sparkles_core::outbound::OutboundPolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A listener answering every request `404` (an S3 endpoint without the object, or a
/// proxy forwarding to one); its port and the connections it accepted.
async fn not_found() -> (u16, Arc<AtomicUsize>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(AtomicUsize::new(0));
    let s2 = seen.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut c, _)) = l.accept().await {
            s2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf).await;
                let _ = c
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });
    (port, seen)
}

#[tokio::test]
async fn policies_bypass_environment_proxies() {
    let (proxy, via_proxy) = not_found().await;
    let (endpoint, direct) = not_found().await;
    // SAFETY: the only test of this binary, and no other thread reads the environment
    // while it is set
    unsafe {
        for v in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
        ] {
            std::env::set_var(v, format!("http://127.0.0.1:{proxy}"));
        }
        for v in ["NO_PROXY", "no_proxy"] {
            std::env::remove_var(v);
        }
        std::env::set_var("SPARKLES_TEST_KEY", "k");
        std::env::set_var("SPARKLES_TEST_SECRET", "s");
    }
    let cfg = RepoConfig {
        name: "s3".into(),
        kind: RepoType::S3,
        bucket: Some("b".into()),
        region: Some("us-east-1".into()),
        endpoint: Some(format!("http://127.0.0.1:{endpoint}")),
        path_style: true,
        allow_http: true,
        credentials: Credentials::Env {
            access_key_id_var: "SPARKLES_TEST_KEY".into(),
            secret_access_key_var: "SPARKLES_TEST_SECRET".into(),
            session_token_var: None,
        },
        ..Default::default()
    };
    let key = object_store::path::Path::from("sparkles-repo.json");

    // under a policy (that admits the endpoint): straight to the endpoint
    let policy = OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    let store = build_store_with(&cfg, Some(&policy)).unwrap();
    let e = store.head(&key).await.unwrap_err();
    assert!(matches!(e, object_store::Error::NotFound { .. }), "{e}");
    assert!(direct.load(Ordering::SeqCst) > 0);
    assert_eq!(via_proxy.load(Ordering::SeqCst), 0);

    // without one: through the environment's proxy
    let before = direct.load(Ordering::SeqCst);
    let store = build_store_with(&cfg, None).unwrap();
    let e = store.head(&key).await.unwrap_err();
    assert!(matches!(e, object_store::Error::NotFound { .. }), "{e}");
    assert!(via_proxy.load(Ordering::SeqCst) > 0);
    assert_eq!(direct.load(Ordering::SeqCst), before);
}
