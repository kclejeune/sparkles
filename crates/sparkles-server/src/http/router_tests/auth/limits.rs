//! The limits around authentication: failures per address before any password is
//! hashed, bounded password work, and device logins per address.

use super::*;
use crate::ratelimit::{RateLimiter, Sources};

/// The fixture server with the rate limits `serve --auth-config … --rate-limit FLAG…`
/// sets up (the pre-authentication limit on by default, callers keyed by owner).
fn limited(f: Fixture, flags: &[&str]) -> AuthServer {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let sources = Sources {
        flags: flags.iter().map(|s| s.to_string()).collect(),
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
