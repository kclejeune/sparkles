//! Unrecorded ranges intersect the request, including a partial stored gap.
use oxrdf::NamedNode;
use sparkles_core::{
    sparql::{QueryOptions, update::update},
    store::{ChangeLogSettings, HistoryBound, HistoryQuery, Store, StoreOptions, UnrecordedReason},
};

fn options() -> StoreOptions {
    StoreOptions {
        change_log_segment_bytes: 4096,
        build: sparkles_core::builder::BuildOptions {
            threads: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn fixture(store: &Store) {
    store.change_log().unwrap().set_background(false);
    store
        .set_change_log_settings(ChangeLogSettings {
            enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
    for n in 1..=8 {
        update(
            store,
            &format!("INSERT DATA {{ <urn:missing:{n}> <urn:lost:p> {n} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    assert_eq!(store.head_commit().seq, 8);
    store
        .set_change_log_settings(ChangeLogSettings::default())
        .unwrap();
    // Seal the segment containing the gap, then leave a different open segment.
    // Reopening the persistent fixture must read its serialized sealed index.
    update(
        store,
        &format!(
            "INSERT DATA {{ <urn:after:9> <urn:recorded:p> \"{}\" }}",
            "x".repeat(5000)
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    update(
        store,
        "INSERT DATA { <urn:after:10> <urn:recorded:p> 10 }",
        &QueryOptions::default(),
    )
    .unwrap();
    store.flush_change_log().unwrap();
    assert_eq!(store.head_commit().seq, 10);
    assert!(store.change_log_status().unwrap().segments >= 2);
    let full = store.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(
        full.changes.len(),
        2,
        "fixture must retain the later recorded changes"
    );
    assert_eq!(
        full.unrecorded
            .iter()
            .map(|r| (r.from, r.to, r.reason))
            .collect::<Vec<_>>(),
        [(1, 8, UnrecordedReason::Gap)]
    );
}

fn assert_windows(store: &Store) {
    let cases = [
        (4, 6),
        (1, 1),
        (0, 1),
        (8, 8),
        (8, 9),
        (9, 9),
        (9, 10),
        (10, 12),
        (11, 12),
        (4, 2),
        (0, 0),
    ];
    for descending in [false, true] {
        for unrelated_filter in [false, true] {
            for (from, to) in cases {
                let result = store
                    .history_changes(&HistoryQuery {
                        from: Some(HistoryBound::Commit(from)),
                        to: Some(HistoryBound::Commit(to)),
                        descending,
                        predicates: if unrelated_filter {
                            vec![NamedNode::new("urn:no-recorded-match").unwrap()]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    })
                    .unwrap();
                let lower = from.max(1);
                let upper = to.min(10);
                let expected_gap = if lower <= upper.min(8) {
                    vec![(lower, upper.min(8), UnrecordedReason::Gap)]
                } else {
                    Vec::new()
                };
                let actual = result
                    .unrecorded
                    .iter()
                    .map(|r| (r.from, r.to, r.reason))
                    .collect::<Vec<_>>();
                assert_eq!(
                    actual, expected_gap,
                    "ending inside a stored gap must report its overlap: {from}..{to}, descending={descending}, unrelated_filter={unrelated_filter}"
                );
                let expected_changes = if unrelated_filter {
                    0
                } else {
                    [9, 10]
                        .into_iter()
                        .filter(|seq| lower <= *seq && *seq <= upper)
                        .count()
                };
                assert_eq!(result.changes.len(), expected_changes);
                assert_eq!((result.from, result.to, result.head), (lower, upper, 10));
            }
        }
    }
    // A selected earlier view must not report a complete empty window merely
    // because the stored gap ends after its selected upper commit.
    let result = store
        .change_log()
        .unwrap()
        .query(
            &HistoryQuery {
                from: Some(HistoryBound::Commit(4)),
                ..Default::default()
            },
            6,
        )
        .unwrap();
    assert!(result.changes.is_empty());
    assert_eq!(
        result
            .unrecorded
            .iter()
            .map(|r| (r.from, r.to, r.reason))
            .collect::<Vec<_>>(),
        [(4, 6, UnrecordedReason::Gap)]
    );
    assert_eq!(result.head, 6);
}

#[test]
fn gap_windows_in_memory() {
    let store = Store::in_memory(options());
    fixture(&store);
    assert_windows(&store);
}

#[test]
fn gap_windows_survive_sealed_index_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), options()).unwrap();
    fixture(&store);
    assert_windows(&store);
    drop(store);
    let store = Store::open(directory.path(), options()).unwrap();
    assert_windows(&store);
}
