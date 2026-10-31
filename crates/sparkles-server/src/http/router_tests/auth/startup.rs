//! The startup states through the auth layer: a caller learns that a dataset is opening
//! or failed only when it may read the dataset, and only a server administrator reads
//! the reason of a failure, which names paths.

use super::*;
use crate::startup::{self, gate::Gate};

const READER_A: char = 'D';
const READER_B: char = 'E';

/// A server whose persistent dataset `bad` does not open and whose `slow` is still
/// opening, with readers of `wiki` (A) and of `bad` and `slow` (B).
fn starting_server(gate: &Arc<Gate>) -> (AuthServer, Arc<AppState>, Router, startup::Startup) {
    let tokens = format!(
        r#"
[[tokens]]
name = "reader-a"
hash = "{}"
datasets = {{ wiki = "read" }}

[[tokens]]
name = "reader-b"
hash = "{}"
datasets = {{ bad = "read", slow = "read" }}
"#,
        token_hash(&tok(READER_A)),
        token_hash(&tok(READER_B))
    );
    let s = build(Fixture {
        extra: tokens,
        ..Default::default()
    });
    for n in ["bad", "slow"] {
        drop(s.state.create(n, DbType::Persistent).unwrap());
    }
    crate::auth::flush(&s.state);
    let AuthServer {
        dir,
        config,
        state,
        app,
        fixture,
    } = s;
    drop(app);
    drop(state);
    std::fs::write(dir.path().join("databases/bad/dataset.json"), b"not json").unwrap();
    let (mut st, pending) = AppState::deferred(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        None,
    )
    .unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let st = Arc::new(st);
    let app = router(st.clone());
    let opening = startup::spawn(
        st.clone(),
        pending,
        startup::Options {
            threads: 2,
            require_all: false,
            before_open: Some(gate.hook()),
        },
        |_, _| Ok(()),
        |e| panic!("fatal: {e}"),
    );
    gate.wait_entered("slow");
    let t0 = std::time::Instant::now();
    while st.catalog.refusal("bad").is_none() {
        assert!(t0.elapsed() < Duration::from_secs(30), "bad never failed");
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = AuthServer {
        dir,
        config,
        state: st.clone(),
        app: app.clone(),
        fixture,
    };
    (s, st, app, opening)
}

fn names(r: &R) -> Vec<String> {
    r.json()["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_states_follow_the_callers_access() {
    let gate = Gate::holding(&["slow"]);
    let (s, _st, app, opening) = starting_server(&gate);
    let dir = s.dir.path().to_str().unwrap().to_string();

    // anonymous callers and readers of other datasets learn nothing of bad or slow
    for who in [None, Some(bearer(&tok(READER_A)))] {
        let who = who.as_deref();
        let r = get_as(&app, "/$/ready", who).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
        let seen = names(&r);
        assert!(
            !seen.iter().any(|n| n == "bad" || n == "slow"),
            "{who:?}: {seen:?}"
        );
        for path in [
            "/bad/sparql?query=ASK%7B%7D",
            "/slow/sparql?query=ASK%7B%7D",
            "/$/ready/bad",
            "/$/ready/slow",
        ] {
            let r = get_as(&app, path, who).await;
            // anonymous callers are asked to sign in, signed-in ones see no dataset
            assert!(
                matches!(r.status, StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND),
                "{who:?} {path}: {} {}",
                r.status,
                r.text()
            );
            assert!(!r.text().contains("opening") && !r.text().contains("failed"));
        }
        let r = get_as(&app, "/$/server", who).await;
        let t = r.text();
        assert!(
            !t.contains("\"bad\"") && !t.contains("\"slow\""),
            "{who:?}: {t}"
        );
    }

    // a reader of both sees their states, and a redacted reason
    let rb = bearer(&tok(READER_B));
    let r = get_as(&app, "/$/ready", Some(&rb)).await;
    let ready = r.json();
    let bad = ready["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "bad")
        .unwrap_or_else(|| panic!("{ready}"));
    assert_eq!(bad["state"], "failed");
    assert_eq!(bad["errorKind"], "corrupt");
    let reason = bad["error"].as_str().unwrap();
    assert!(
        !reason.contains(&dir) && !reason.contains("not json"),
        "{reason}"
    );
    let r = get_as(&app, "/bad/sparql?query=ASK%7B%7D", Some(&rb)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["code"], "dataset-failed");
    assert!(!r.text().contains(&dir), "{}", r.text());
    let r = get_as(&app, "/slow/sparql?query=ASK%7B%7D", Some(&rb)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json()["code"], "dataset-opening");
    let r = get_as(&app, "/$/ready/slow", Some(&rb)).await;
    assert_eq!(r.json()["datasets"][0]["state"], "opening", "{}", r.text());
    // a write needs more than read: forbidden, as for an open dataset
    let r = update_as(&app, "slow", &rb, "INSERT DATA { <urn:a> <urn:b> 1 }").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = get_as(&app, "/$/server", Some(&rb)).await;
    let srv = r.json();
    assert_eq!(srv["openingDatasets"], serde_json::json!(["slow"]), "{srv}");
    assert_eq!(srv["refusedDatasets"][0]["name"], "bad", "{srv}");
    assert_eq!(
        srv["refusedDatasets"][0]["reason"],
        crate::state::redacted_refusal("corrupt"),
        "{srv}"
    );

    // a server administrator reads the whole reason
    let r = get_as(&app, "/$/ready", Some(&b("alice"))).await;
    let ready = r.json();
    let bad = ready["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "bad")
        .unwrap();
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("dataset \"bad\" is not served"),
        "{ready}"
    );
    // the metrics name no dataset of these states, and keep their permission
    let r = get_as(&app, "/$/metrics", Some(&rb)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    gate.release("slow");
    opening.stop_and_join();
}
