//! The limits around authentication: failures per address before any password is
//! hashed, bounded password work, and quotas per token owner.

use super::tokens::mint_as;
use super::*;
use crate::ratelimit::{RateLimiter, Sources};

/// The fixture server with the rate limits `serve --auth-config … --rate-limit FLAG…`
/// sets up (the pre-authentication limit on by default, callers keyed by owner).
fn limited(f: Fixture, flags: &[&str]) -> AuthServer {
    limited_behind(f, flags, &[])
}

/// [`limited`] with `--rate-limit-trusted-proxy PROXY…`.
fn limited_behind(f: Fixture, flags: &[&str], proxies: &[&str]) -> AuthServer {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let sources = Sources {
        flags: flags.iter().map(|s| s.to_string()).collect(),
        trusted_proxies: proxies.iter().map(|s| s.to_string()).collect(),
        auth: true,
        ..Default::default()
    };
    let cfg = sources.load().unwrap().unwrap();
    st.rate_limit = Some(Arc::new(
        RateLimiter::new(&cfg)
            .unwrap()
            .with_keyer(Arc::new(crate::auth::PrincipalKeyer)),
    ));
    let st = Arc::new(st);
    for name in ["wiki", "public"] {
        let ds = st.attach(name, DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                format!("<http://ex.org/{name}> <http://ex.org/p> \"1\" .").into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st.clone());
    AuthServer {
        dir,
        config,
        state: st,
        app,
        fixture: f,
    }
}

fn from(ip: &str) -> Peer {
    Peer::Tcp(format!("{ip}:1").parse().unwrap())
}

async fn ask_from(s: &AuthServer, ip: &str, auth: &str) -> R {
    let wiki = format!("/wiki{ASK}");
    call_from(
        &s.app,
        from(ip),
        "GET",
        &wiki,
        &[("authorization", auth)],
        "",
    )
    .await
}

/// The sum of a metric over the lines that contain all of `parts`.
async fn metric_sum(app: &Router, parts: &[&str]) -> u64 {
    get_as(app, "/$/metrics", Some(&bearer(&t_prom())))
        .await
        .text()
        .lines()
        .filter(|l| !l.starts_with('#') && parts.iter().all(|p| l.contains(p)))
        .filter_map(|l| l.rsplit(' ').next()?.parse::<u64>().ok())
        .sum()
}

#[tokio::test]
async fn failed_logins_spend_the_address_budget_before_hashing() {
    let s = limited(Fixture::default(), &["preauth=3/min"]);
    let wrong = basic("bob", "wrong");
    for i in 0..3 {
        let r = ask_from(&s, "203.0.113.1", &wrong).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{i}");
        assert_eq!(
            r.header("ratelimit"),
            format!("\"preauth\";r={};t={}", 2 - i, 20 * (i + 1))
        );
    }
    let hashed = || metric(&s.app, "sparkles_auth_password_verifications_total");
    assert_eq!(hashed().await, 3);
    // spent: refused before the password is looked at, even the right one
    let r = ask_from(&s, "203.0.113.1", &wrong).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.header("retry-after"), "20");
    assert_eq!(r.json()["limitClass"], "preauth");
    let r = ask_from(&s, "203.0.113.1", &b("bob")).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hashed().await, 3);
    // other addresses are not affected
    assert_eq!(
        ask_from(&s, "203.0.113.2", &b("bob")).await.status,
        StatusCode::OK
    );
    // successes cost nothing: the right password many times over
    for _ in 0..10 {
        assert_eq!(
            ask_from(&s, "203.0.113.3", &b("bob")).await.status,
            StatusCode::OK
        );
    }
    // bearer tokens and UI password logins spend the same budget
    let junk = bearer(&tok('Z'));
    for _ in 0..3 {
        let r = ask_from(&s, "203.0.113.4", &junk).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        ask_from(&s, "203.0.113.4", &junk).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let app = &s.app;
    let login = |ip: &'static str| async move {
        call_from(
            app,
            from(ip),
            "POST",
            "/$/auth/login",
            &[("content-type", "application/json")],
            r#"{"user":"bob","password":"wrong"}"#,
        )
        .await
        .status
    };
    for _ in 0..3 {
        assert_eq!(login("203.0.113.5").await, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(login("203.0.113.5").await, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        metric_sum(
            &s.app,
            &["sparkles_rate_limited_total{", "class=\"preauth\""]
        )
        .await
            >= 4
    );
}

/// Wait (up to ten seconds) for `f`.
async fn until(mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn password_checks_wait_in_a_bounded_queue() {
    let s = limited(Fixture::default(), &[]);
    let auth = s.auth();
    let (permits, queue) = auth.verification_queue();
    // every permit is taken: checks queue, and those beyond the queue are refused at once
    let hold = auth.hold_verifications();
    let extra = 5;
    let tasks: Vec<_> = (0..queue + extra)
        .map(|i| {
            let app = s.app.clone();
            tokio::spawn(async move {
                let wrong = basic("bob", &format!("wrong-{i}"));
                let ip = format!("198.51.{}.{}:1", i / 200, i % 200 + 1);
                let wiki = format!("/wiki{ASK}");
                let peer = Peer::Tcp(ip.parse().unwrap());
                call_from(&app, peer, "GET", &wiki, &[("authorization", &wrong)], "")
                    .await
                    .status
            })
        })
        .collect();
    until(|| {
        auth.argon_load().1 == queue && tasks.iter().filter(|t| t.is_finished()).count() == extra
    })
    .await;
    assert_eq!(auth.argon_load(), (permits, queue));
    assert_eq!(
        metric(&s.app, "sparkles_auth_password_verifications_total").await,
        0
    );
    assert_eq!(
        metric(
            &s.app,
            "sparkles_auth_failures_total{scheme=\"basic\",reason=\"busy\"}"
        )
        .await,
        extra as u64
    );
    assert_eq!(
        metric(&s.app, "sparkles_auth_password_verifications_waiting").await,
        queue as u64
    );
    drop(hold);
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    let count = |c: StatusCode| statuses.iter().filter(|s| **s == c).count();
    assert_eq!(count(StatusCode::SERVICE_UNAVAILABLE), extra);
    assert_eq!(count(StatusCode::UNAUTHORIZED), queue);
    assert_eq!(auth.argon_load(), (0, 0));
    assert_eq!(
        metric(&s.app, "sparkles_auth_password_verifications_total").await,
        queue as u64
    );
}

#[tokio::test]
async fn concurrent_guesses_from_one_address_stop_at_its_budget() {
    let s = limited(Fixture::default(), &["preauth=3/min"]);
    let auth = s.auth();
    let hold = auth.hold_verifications();
    let tasks: Vec<_> = (0..10)
        .map(|i| {
            let app = s.app.clone();
            tokio::spawn(async move {
                let wrong = basic("bob", &format!("wrong-{i}"));
                let wiki = format!("/wiki{ASK}");
                call_from(
                    &app,
                    from("203.0.113.9"),
                    "GET",
                    &wiki,
                    &[("authorization", &wrong)],
                    "",
                )
                .await
                .status
            })
        })
        .collect();
    // three reserve a failure and wait for a permit; the others are refused unhashed
    // (when their reservation fails, or at admission once the reservations are made)
    until(|| auth.argon_load().1 == 3 && tasks.iter().filter(|t| t.is_finished()).count() == 7)
        .await;
    let refused = ["sparkles_rate_limited_total{", "class=\"preauth\""];
    assert_eq!(metric_sum(&s.app, &refused).await, 7);
    assert_eq!(
        metric(&s.app, "sparkles_auth_password_verifications_total").await,
        0
    );
    drop(hold);
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    let count = |c: StatusCode| statuses.iter().filter(|s| **s == c).count();
    assert_eq!(count(StatusCode::TOO_MANY_REQUESTS), 7);
    assert_eq!(count(StatusCode::UNAUTHORIZED), 3);
    assert_eq!(
        metric(&s.app, "sparkles_auth_password_verifications_total").await,
        3
    );
}

#[tokio::test]
async fn the_tokens_of_one_owner_share_its_quota() {
    let s = limited(Fixture::default(), &["query=3/min"]);
    let mut tokens = Vec::new();
    for name in ["a", "b"] {
        let body = format!(r#"{{"name":"{name}","datasets":{{"wiki":"read"}}}}"#);
        let r = mint_as(&s.app, &[("authorization", &b("bob"))], &body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        tokens.push(bearer(r.json()["token"].as_str().unwrap()));
    }
    // three queries a minute for bob, whichever of his credentials and addresses
    let bob = [&tokens[0], &tokens[1], &b("bob")];
    for (i, auth) in bob.iter().enumerate() {
        let r = ask_from(&s, &format!("192.0.2.{}", i + 1), auth).await;
        assert_eq!(r.status, StatusCode::OK, "{i}");
    }
    for (i, auth) in bob.iter().enumerate() {
        let r = ask_from(&s, &format!("192.0.2.{}", i + 1), auth).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{i}");
        assert_eq!(r.json()["limitClass"], "query");
    }
    // his session too
    let (cookie, _) = password_session(&s, "bob").await;
    let wiki = format!("/wiki{ASK}");
    let r = call(&s.app, "GET", &wiki, &[("cookie", &cookie)], "").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // a static token of the configuration, and other users, have their own budgets
    assert_eq!(
        ask_from(&s, "192.0.2.1", &bearer(&t_etl())).await.status,
        StatusCode::OK
    );
    assert_eq!(
        ask_from(&s, "192.0.2.1", &b("alice")).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn owners_have_a_token_cap_and_a_mint_rate() {
    let s = build(Fixture {
        extra: "[tokens_policy]\nmax_active_per_owner = 2\nmint_rate = \"4/h\"\n".into(),
        ..Default::default()
    });
    let bob = [("authorization", b("bob"))];
    let bob: Vec<(&str, &str)> = bob.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mint = |name: &'static str| {
        let (app, bob) = (&s.app, &bob);
        async move { mint_as(app, bob, &format!(r#"{{"name":"{name}"}}"#)).await }
    };
    let a = mint("a").await;
    assert_eq!(a.status, StatusCode::CREATED);
    assert_eq!(mint("b").await.status, StatusCode::CREATED);
    let r = mint("c").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(r.text().contains("at most 2 active tokens"), "{}", r.text());
    // revoking makes room
    let id = a.json()["id"].as_str().unwrap().to_string();
    let r = call(&s.app, "DELETE", &format!("/$/auth/tokens/{id}"), &bob, "").await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(mint("d").await.status, StatusCode::CREATED);
    // four mints an hour (the refused one counted)
    let r = mint("e").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.header("ratelimit-policy"), "\"mint\";q=4;w=3600");
    assert_eq!(r.json()["reason"], "mint");
    // per owner: alice is not affected
    let alice = [("authorization", b("alice"))];
    let alice: Vec<(&str, &str)> = alice.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = mint_as(&s.app, &alice, r#"{"name":"x"}"#).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert!(metric_sum(&s.app, &["sparkles_rate_limit_keys{limiter=\"auth\"}"]).await >= 2);
}

#[tokio::test]
async fn device_logins_are_limited_per_address() {
    let s = limited(Fixture::default(), &[]);
    let app = &s.app;
    let start = |ip: &'static str| async move {
        call_from(
            app,
            from(ip),
            "POST",
            "/$/auth/device",
            &[("content-type", "application/x-www-form-urlencoded")],
            "",
        )
        .await
    };
    for _ in 0..20 {
        assert_eq!(start("203.0.113.7").await.status, StatusCode::OK);
    }
    let r = start("203.0.113.7").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json()["reason"], "device");
    assert_eq!(start("203.0.113.8").await.status, StatusCode::OK);
}

/// A failed Basic login to `/wiki` from `peer` with `headers`.
async fn guess_from(s: &AuthServer, peer: Peer, headers: &[(&str, &str)], pw: &str) -> R {
    let wrong = basic("bob", pw);
    let mut h = headers.to_vec();
    h.push(("authorization", &wrong));
    call_from(&s.app, peer, "GET", &format!("/wiki{ASK}"), &h, "").await
}

#[tokio::test]
async fn a_client_cannot_pick_its_budget_with_forwarded() {
    let s = limited_behind(Fixture::default(), &["preauth=30/min"], &["127.0.0.1"]);
    let proxy = || from("127.0.0.1");
    // one real client behind the proxy, a new Forwarded value with every guess
    for i in 0..30 {
        let fwd = format!("for=198.51.100.{i}");
        let h = [
            ("x-forwarded-for", "203.0.113.50"),
            ("forwarded", fwd.as_str()),
        ];
        let r = guess_from(&s, proxy(), &h, &format!("wrong-{i}")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{i}");
    }
    let h = [
        ("x-forwarded-for", "203.0.113.50"),
        ("forwarded", "for=198.51.100.200"),
    ];
    let r = guess_from(&s, proxy(), &h, "wrong-30").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // another client behind the same proxy still signs in
    let wiki = format!("/wiki{ASK}");
    let bob = b("bob");
    let h = [
        ("x-forwarded-for", "203.0.113.51"),
        ("authorization", bob.as_str()),
    ];
    let r = call_from(&s.app, proxy(), "GET", &wiki, &h, "").await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn unix_socket_clients_are_told_apart_by_a_trusted_proxy() {
    let s = limited_behind(Fixture::default(), &["preauth=3/min"], &["unix"]);
    let one = [("x-forwarded-for", "203.0.113.1")];
    for i in 0..3 {
        let r = guess_from(&s, Peer::Unix, &one, &format!("wrong-{i}")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{i}");
    }
    let r = guess_from(&s, Peer::Unix, &one, "wrong-3").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // another address behind the proxy is not affected
    let two = [("x-forwarded-for", "203.0.113.2")];
    let r = guess_from(&s, Peer::Unix, &two, "wrong").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_untrusted_unix_socket_shares_one_budget() {
    let s = limited(Fixture::default(), &["preauth=3/min"]);
    let one = [("x-forwarded-for", "203.0.113.1")];
    for i in 0..3 {
        let r = guess_from(&s, Peer::Unix, &one, &format!("wrong-{i}")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{i}");
    }
    // guessing is bounded on the socket too: its clients share one budget
    let two = [("x-forwarded-for", "203.0.113.2")];
    let r = guess_from(&s, Peer::Unix, &two, "wrong").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        metric(
            &s.app,
            "sparkles_rate_limit_untrusted_forwarded_total{limiter=\"requests\"}"
        )
        .await,
        4
    );
}
