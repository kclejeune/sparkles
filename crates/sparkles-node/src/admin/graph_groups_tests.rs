use super::*;

fn dataset() -> (Dataset, Arc<sparkles::sparql::pool::QueryPool>) {
    let pool = sparkles::sparql::pool::QueryPool::new(64 << 20);
    let ds = Dataset::from_store_with(
        sparkles::store::Store::in_memory(Default::default()),
        sparkles::DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    let rows = (0..24)
        .map(|i| format!("<urn:s{i}> a <urn:T> ; <urn:region> <urn:r{}> .", i % 12))
        .collect::<String>();
    ds.update(&format!("INSERT DATA {{ {rows} }}")).unwrap();
    (ds, pool)
}

fn groups(
    ds: &Dataset,
    flag: Arc<AtomicBool>,
    deadline: Option<Instant>,
) -> sparkles::Result<Value> {
    run_admin(
        ds,
        "graph.groups",
        &json!({"request":{"within":{"class":"urn:T"},"by":{"property":"urn:region"}}}),
        // Each individual query has ample time. The original operation's deadline
        // must still win, rather than granting another relative timeout per query.
        &json!({"timeout":3600000,"noCache":true}),
        flag.clone(),
        &sparkles::task::Control {
            deadline,
            ..sparkles::task::Control::with_cancel(flag)
        },
    )
}

#[test]
fn graph_groups_keeps_expired_operation_deadline_despite_fresh_query_timeout() {
    let (ds, pool) = dataset();
    let result = groups(
        &ds,
        Arc::new(AtomicBool::new(false)),
        Some(Instant::now() - Duration::from_secs(1)),
    );
    assert!(
        matches!(result, Err(EngineError::Timeout)),
        "the original operation deadline was renewed: {result:?}"
    );
    assert_eq!(pool.granted(), 0);
}

#[test]
fn graph_groups_live_operation_deadlines_preserve_answers_and_release_grants() {
    let (ds, pool) = dataset();
    let unbounded = groups(&ds, Arc::new(AtomicBool::new(false)), None).unwrap();
    let bounded = groups(
        &ds,
        Arc::new(AtomicBool::new(false)),
        Some(Instant::now() + Duration::from_secs(3600)),
    )
    .unwrap();
    assert_eq!(bounded, unbounded);
    assert_eq!(bounded["within"]["size"], 24);
    let groups = bounded["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 12);
    assert!(groups.iter().all(|group| group["size"] == 2));
    assert_eq!(pool.granted(), 0);
}

#[test]
fn graph_groups_preserves_cancellation_with_an_operation_deadline() {
    let (ds, pool) = dataset();
    let result = groups(
        &ds,
        Arc::new(AtomicBool::new(true)),
        Some(Instant::now() + Duration::from_secs(3600)),
    );
    assert!(matches!(result, Err(EngineError::Cancelled)), "{result:?}");
    assert_eq!(pool.granted(), 0);
}
