use sparkles::{
    Dataset, Error,
    history::{At, HistoryOptions},
    sparql::query,
};

#[test]
fn trusted_dataset_history_uses_real_caps_and_preserves_retained_origin() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:one> <urn:p> 1 }").unwrap();
    ds.create_branch("parent", &Default::default()).unwrap();
    let parent = ds.branch("parent").unwrap();
    parent
        .update("INSERT DATA { <urn:two> <urn:p> _:b }")
        .unwrap();
    let commit = parent.store().head_commit();
    let id = parent.store().branch_id();
    parent
        .update("INSERT DATA { <urn:later> <urn:p> 3 }")
        .unwrap();
    ds.create_branch(
        "child",
        &sparkles::branch::BranchOptions {
            from: "parent".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let child = ds.branch("child").unwrap();
    child
        .update("INSERT DATA { <urn:child> <urn:p> 4 }")
        .unwrap();
    drop(child);
    drop(parent);
    drop(ds);
    let ds = Dataset::open(dir.path()).unwrap();
    let child = ds.branch("child").unwrap();
    let (snapshot, resolved, origin) = ds
        .history()
        .snapshot_at_with_branch_limited(
            &child,
            &At::Commit(commit.seq),
            &HistoryOptions::default(),
            32 << 20,
        )
        .unwrap();
    assert_eq!(resolved.commit, commit);
    assert_eq!(origin, id);
    assert_eq!(
        query(snapshot, "SELECT * { ?s ?p ?o }", &Default::default())
            .unwrap()
            .rows()
            .len(),
        2
    );
    assert!(matches!(
        ds.history().snapshot_at_with_branch_limited(
            &child,
            &At::Commit(commit.seq),
            &HistoryOptions::default(),
            1024
        ),
        Err(Error::BudgetExceeded(_))
    ));
    let (live, resolved) = ds
        .history()
        .snapshot_at_limited(&At::Head, &Default::default(), 32 << 20)
        .unwrap();
    assert_eq!(live.commit, resolved.commit.seq);
    assert!(matches!(
        ds.history()
            .snapshot_at_limited(&At::Head, &Default::default(), 1024),
        Err(Error::BudgetExceeded(_))
    ));
    let foreign = Dataset::memory();
    assert!(matches!(
        ds.history().snapshot_at_with_branch_limited(
            &foreign,
            &At::Head,
            &Default::default(),
            32 << 20
        ),
        Err(Error::Invalid(_))
    ));
}
