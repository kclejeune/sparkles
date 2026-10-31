//! Expiry of idle scratch branches (spec C17 §5.7, roadmap item 57): the dataset
//! setting `memory.scratchBranches.ttl` over the server's default, and the passes of
//! `sparkles mcp`. Every test sets its own clock, so none waits for a branch to age.

use super::branches::{self, Cadence, Clock};
use crate::state::{AppState, DbType};
use serde_json::json;
use sparkles::branch::Scratch;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

const HOUR_MS: i64 = 3_600_000;

fn state() -> Arc<AppState> {
    Arc::new(AppState::standalone(
        Default::default(),
        Duration::from_secs(30),
    ))
}

/// A dataset `name` with the scratch branch `scratch` made by `creator`, and the time it
/// was created.
fn dataset_with_scratch(st: &Arc<AppState>, name: &str, setting: Option<&str>) -> i64 {
    let ds = st.attach(name, DbType::Mem, None).unwrap();
    if let Some(ttl) = setting {
        crate::assist::write_file(
            st,
            &ds,
            "memory.json",
            &json!({"scratchBranches": {"ttl": ttl}}),
        )
        .unwrap();
    }
    let b = ds
        .store
        .create_branch("scratch", &Default::default())
        .unwrap();
    ds.store
        .set_branch_scratch(
            "scratch",
            Some(Scratch {
                creator: "agent".into(),
            }),
        )
        .unwrap();
    b.created_ms
}

fn has_scratch(st: &AppState, name: &str) -> bool {
    st.get(name).unwrap().store.branch_info("scratch").is_ok()
}

/// A dataset's setting replaces the server's default, in both directions, and `off`
/// keeps the branches although the server sets a duration.
#[test]
fn the_dataset_setting_overrides_the_server_default() {
    let st = state();
    let day = Duration::from_secs(86_400);
    let t = dataset_with_scratch(&st, "short", Some("1h"));
    dataset_with_scratch(&st, "plain", None);
    dataset_with_scratch(&st, "long", Some("90d"));
    dataset_with_scratch(&st, "never", Some("off"));
    // two hours on: only the dataset whose own TTL is one hour expires, although the
    // server's default is a day
    let gone = branches::sweep(&st, day, t + 2 * HOUR_MS);
    assert_eq!(gone, ["short@scratch"]);
    assert!(!has_scratch(&st, "short"));
    for n in ["plain", "long", "never"] {
        assert!(has_scratch(&st, n), "{n}");
    }
    // two days on: the default applies to a dataset without a setting, and the longer
    // setting keeps its branch beyond the default
    let gone = branches::sweep(&st, day, t + 48 * HOUR_MS);
    assert_eq!(gone, ["plain@scratch"]);
    assert!(has_scratch(&st, "long") && has_scratch(&st, "never"));
    // without a server default only the settings decide, and off never expires
    let gone = branches::sweep(&st, None, t + 91 * 24 * HOUR_MS);
    assert_eq!(gone, ["long@scratch"]);
    let gone = branches::sweep(&st, day, t + 10_000 * HOUR_MS);
    assert!(gone.is_empty(), "{gone:?}");
    assert!(has_scratch(&st, "never"));
}

/// A ttl that does not validate leaves the decision to the server's default.
#[test]
fn an_invalid_stored_value_falls_back_to_the_default() {
    let st = state();
    let t = dataset_with_scratch(&st, "d", Some("soon"));
    let ds = st.get("d").unwrap();
    let day = Duration::from_secs(86_400);
    assert_eq!(branches::dataset_ttl(&st, &ds, Some(day)), Some(day));
    assert_eq!(branches::dataset_ttl(&st, &ds, None), None);
    assert!(branches::sweep(&st, None, t + 1000 * HOUR_MS).is_empty());
}

/// A change of the setting applies at the next pass, with no restart.
#[test]
fn a_changed_setting_applies_at_the_next_pass() {
    let st = state();
    let t = dataset_with_scratch(&st, "d", Some("30d"));
    assert!(branches::sweep(&st, None, t + 3 * HOUR_MS).is_empty());
    let ds = st.get("d").unwrap();
    crate::assist::write_file(
        &st,
        &ds,
        "memory.json",
        &json!({"scratchBranches": {"ttl": "2h"}}),
    )
    .unwrap();
    assert_eq!(branches::sweep(&st, None, t + 3 * HOUR_MS), ["d@scratch"]);
}

/// The stdio session expires abandoned branches before it serves, from the dataset's
/// setting alone, and then at its cadence, under a clock the test moves.
#[tokio::test(flavor = "multi_thread")]
async fn stdio_expires_on_open_and_then_on_a_cadence() {
    let st = state();
    let t = dataset_with_scratch(&st, "d", Some("1h"));
    let clock_ms = Arc::new(AtomicI64::new(t + 2 * HOUR_MS));
    let clock: Clock = {
        let c = clock_ms.clone();
        Arc::new(move || c.load(Ordering::SeqCst))
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let st2 = st.clone();
    let c2 = clock.clone();
    let opening = tokio::task::spawn_blocking(move || {
        branches::expire_for_stdio(
            &st2,
            &tokio::runtime::Handle::current(),
            Duration::from_millis(20),
            c2,
            Some(tx),
        )
    })
    .await
    .unwrap();
    // the opening pass ran before the call returned
    assert_eq!(opening, ["d@scratch"]);
    assert!(!has_scratch(&st, "d"));
    // a branch made later is idle only once the clock has moved past its TTL, whatever
    // the number of passes in between
    clock_ms.store(t, Ordering::SeqCst);
    let ds = st.get("d").unwrap();
    let later = ds
        .store
        .create_branch("scratch", &Default::default())
        .unwrap();
    ds.store
        .set_branch_scratch(
            "scratch",
            Some(Scratch {
                creator: "agent".into(),
            }),
        )
        .unwrap();
    clock_ms.store(later.created_ms + HOUR_MS / 2, Ordering::SeqCst);
    let pass = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(pass.is_empty(), "{pass:?}");
    assert!(has_scratch(&st, "d"));
    clock_ms.store(later.created_ms + 2 * HOUR_MS, Ordering::SeqCst);
    loop {
        let pass = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if !pass.is_empty() {
            assert_eq!(pass, ["d@scratch"]);
            break;
        }
    }
    assert!(!has_scratch(&st, "d"));
}

/// The server's passes cover a dataset created after they started and need no flag.
#[tokio::test(flavor = "multi_thread")]
async fn server_passes_read_settings_of_datasets_created_later() {
    let st = state();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let clock_ms = Arc::new(AtomicI64::new(0));
    let c = clock_ms.clone();
    let handle = branches::spawn_expiry_with(
        st.clone(),
        None,
        Cadence::Every(Duration::from_millis(20)),
        Arc::new(move || c.load(Ordering::SeqCst)),
        Some(tx),
    );
    assert!(rx.recv().await.unwrap().is_empty());
    let t = dataset_with_scratch(&st, "late", Some("1h"));
    clock_ms.store(t + 2 * HOUR_MS, Ordering::SeqCst);
    loop {
        let pass = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if !pass.is_empty() {
            assert_eq!(pass, ["late@scratch"]);
            break;
        }
    }
    handle.abort();
}

/// The server's sweeper deletes branches, so it starts only when the operator allowed
/// MCP updates, whether the duration comes from the flag or from a dataset setting.
#[tokio::test(flavor = "multi_thread")]
async fn the_server_sweeper_needs_mcp_update_permission() {
    let conf = |allow_update: bool, scratch_ttl: Option<Duration>| {
        Arc::new(crate::mcp::http::HttpConf {
            cfg: crate::mcp::McpConfig {
                allow_update,
                scratch_ttl,
                ..Default::default()
            },
            max_sessions: 0,
            shutdown: Default::default(),
            require_auth: false,
        })
    };
    let hour = Some(Duration::from_secs(3600));
    for (allow, ttl, read_only, starts) in [
        (false, None, false, false),
        (false, hour, false, false),
        (true, hour, true, false),
        (true, None, false, true),
        (true, hour, false, true),
    ] {
        let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
        st.read_only = read_only;
        st.mcp = Some(conf(allow, ttl));
        // a dataset with its own duration, so a started sweeper has work to do
        let st = Arc::new(st);
        dataset_with_scratch(&st, "d", Some("1h"));
        assert_eq!(
            branches::spawn_server_expiry(&st),
            starts,
            "allow_update {allow}, flag {ttl:?}, read_only {read_only}"
        );
    }
    // without the endpoint nothing starts either
    let st = Arc::new(AppState::standalone(
        Default::default(),
        Duration::from_secs(30),
    ));
    assert!(!branches::spawn_server_expiry(&st));
}

/// `serve` binds its listener and opens the datasets afterwards. The first pass of the
/// server's sweeper waits for them, since one made before finds no dataset and the next
/// would come a minute later.
#[tokio::test(flavor = "multi_thread")]
async fn the_server_sweeper_waits_for_the_datasets_to_open() {
    let st = state();
    st.set_phase(crate::obs::Phase::Starting);
    let t = dataset_with_scratch(&st, "d", Some("1h"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = branches::spawn_expiry_with(
        st.clone(),
        None,
        Cadence::Adaptive,
        Arc::new(move || t + 2 * HOUR_MS),
        Some(tx),
    );
    // nothing runs while the server is starting
    assert!(
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err()
    );
    assert!(has_scratch(&st, "d"));
    assert!(st.mark_ready());
    let pass = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pass, ["d@scratch"]);
    handle.abort();
}
