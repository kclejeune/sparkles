//! Trusted-header authentication behind a forward-auth proxy.

use super::*;

const PROXY: &str = r#"
[proxy]
preset = "authelia"
trusted = ["127.0.0.1/32", "unix"]
logout_url = "https://auth.example.org/logout"
"#;

fn proxy_server() -> AuthServer {
    build(Fixture {
        extra: PROXY.into(),
        ..Default::default()
    })
}

fn trusted() -> Peer {
    Peer::Tcp("127.0.0.1:40000".parse().unwrap())
}

const DAVE: [(&str, &str); 2] = [
    ("remote-user", "dave"),
    ("remote-groups", "sparkles,kg-editors"),
];

#[tokio::test]
async fn trusted_peer_headers_authenticate() {
    let s = proxy_server();
    let who = call_from(&s.app, trusted(), "GET", "/$/whoami", &DAVE, "").await;
    assert_eq!(who.status, StatusCode::OK);
    let w = who.json();
    assert_eq!(w["principal"]["kind"], "proxy");
    assert_eq!(w["principal"]["name"], "dave");
    assert_eq!(w["method"], "proxy");
    assert_eq!(w["datasets"]["wiki"], "write");
    assert_eq!(w["logout"], true);
    let csrf = w["csrfToken"].as_str().unwrap().to_string();

    let mut h = DAVE.to_vec();
    h.push(("content-type", "application/sparql-update"));
    let before = head(&s.state, "wiki");
    let no_csrf = call_from(&s.app, trusted(), "POST", "/wiki/update", &h, INSERT).await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    assert_eq!(no_csrf.json()["error"], "CSRF token missing or invalid");
    assert_eq!(head(&s.state, "wiki"), before);
    h.push(("x-sparkles-csrf", &csrf));
    let ok = call_from(&s.app, trusted(), "POST", "/wiki/update", &h, INSERT).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());

    // logout answers the proxy's logout page
    let lo = call_from(
        &s.app,
        trusted(),
        "POST",
        "/$/auth/logout",
        &[
            ("remote-user", "dave"),
            ("remote-groups", "sparkles"),
            ("x-sparkles-csrf", &csrf),
        ],
        "",
    )
    .await;
    assert_eq!(lo.json()["redirect"], "https://auth.example.org/logout");
}

#[tokio::test]
async fn untrusted_peers_are_ignored() {
    let s = proxy_server();
    let r = call(&s.app, "GET", &format!("/wiki{ASK}"), &DAVE, "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let who = call(&s.app, "GET", "/$/whoami", &DAVE, "").await.json();
    assert_eq!(who["principal"]["kind"], "anonymous");
    assert!(metric(&s.app, "sparkles_auth_untrusted_proxy_headers_total").await >= 2);
    let unix = call_from(&s.app, Peer::Unix, "GET", "/$/whoami", &DAVE, "")
        .await
        .json();
    assert_eq!(unix["principal"]["kind"], "proxy");
    // a request without a known peer is never trusted
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get("/$/whoami")
                .header("remote-user", "dave")
                .header("remote-groups", "sparkles")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(j["principal"]["kind"], "anonymous");
}

#[tokio::test]
async fn admission_and_precedence() {
    let s = proxy_server();
    let outsider = [("remote-user", "eve"), ("remote-groups", "kg-editors")];
    let r = call_from(
        &s.app,
        trusted(),
        "GET",
        &format!("/wiki{ASK}"),
        &outsider,
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"], "user not allowed");
    // public routes stay reachable
    let ping = call_from(&s.app, trusted(), "GET", "/$/ping", &outsider, "").await;
    assert_eq!(ping.status, StatusCode::OK);
    // Authorization wins over proxy headers
    let mut h = DAVE.to_vec();
    let bob = b("bob");
    h.push(("authorization", &bob));
    let who = call_from(&s.app, trusted(), "GET", "/$/whoami", &h, "")
        .await
        .json();
    assert_eq!(who["principal"]["name"], "bob");
    assert_eq!(who["principal"]["kind"], "user");
    // a user header that is not visible ASCII is ignored
    let odd = [("remote-user", "a b"), ("remote-groups", "sparkles")];
    let who = call_from(&s.app, trusted(), "GET", "/$/whoami", &odd, "")
        .await
        .json();
    assert_eq!(who["principal"]["kind"], "anonymous");
}

#[test]
fn proxy_configuration_errors_and_warnings() {
    use crate::auth::config::FileConfig;
    let base = config_text(&Fixture::default());
    let with = |p: &str| format!("{base}\n[proxy]\npreset = \"authelia\"\ntrusted = [{p}]\n");
    let e = FileConfig::parse(&with("\"0.0.0.0/0\"")).unwrap_err();
    assert!(e.to_string().contains("every address"), "{e}");
    let e = FileConfig::parse(&with("\"::/0\"")).unwrap_err();
    assert!(e.to_string().contains("every address"), "{e}");
    let e = FileConfig::parse(&with("\"not-a-cidr\"")).unwrap_err();
    assert!(e.to_string().contains("CIDR"), "{e}");
    let cfg = FileConfig::parse(&with("\"10.0.0.0/8\"")).unwrap();
    let w = cfg.warnings();
    assert!(
        w.iter()
            .any(|w| w.contains("10.0.0.0/8") && w.contains("Remote-User")),
        "{w:?}"
    );
    let e =
        FileConfig::parse(&format!("{base}\n[proxy]\ntrusted = [\"127.0.0.1\"]\n")).unwrap_err();
    assert!(e.to_string().contains("user_header"), "{e}");
}

#[tokio::test]
async fn local_proxy_headers_need_a_known_host() {
    let f = Fixture {
        extra: PROXY.into(),
        public_url: "https://sparql.example.org",
        ..Default::default()
    };
    let s = build(f.clone());
    let who = |peer: Peer, host: &'static str| {
        let app = s.app.clone();
        async move {
            let mut h = DAVE.to_vec();
            h.push(("host", host));
            call_from(&app, peer, "GET", "/$/whoami", &h, "").await
        }
    };
    // a page that rebinds its name to the server sends its own Host: refused
    let r = who(trusted(), "evil.example").await;
    assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{}", r.text());
    let r = who(Peer::Unix, "evil.example:80").await;
    assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST);
    // the public URL's host, IP addresses and localhost are known
    for host in [
        "sparql.example.org",
        "SPARQL.example.org:443",
        "127.0.0.1:3030",
        "localhost:3030",
        "[::1]",
    ] {
        let r = who(trusted(), host).await;
        assert_eq!(r.status, StatusCode::OK, "{host}");
        assert_eq!(r.json()["principal"]["kind"], "proxy", "{host}");
    }
    // without identity headers any Host is answered, as with auth before
    let h = [("host", "evil.example")];
    let r = call_from(&s.app, trusted(), "GET", "/$/whoami", &h, "").await;
    assert_eq!(r.status, StatusCode::OK);
    // the startup warning: no host name of its own (no public URL, no --public-host)
    assert!(crate::auth::proxy_host_warning(&s.state, false).is_none());
    let without = config_text(&f).replace("public_url = \"https://sparql.example.org\"", "");
    std::fs::write(&s.config, without).unwrap();
    s.auth().reload().unwrap();
    let w = crate::auth::proxy_host_warning(&s.state, false).unwrap();
    assert!(w.contains("--public-host"), "{w}");
    assert!(crate::auth::proxy_host_warning(&s.state, true).is_none());
}
