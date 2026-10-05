//! Catalog rename through HTTP: identity, routing, live views and reservations.
use super::*;

async fn rename(app: &Router, source: &str, target: &str) -> Resp {
    send(
        app,
        Request::post(format!("/$/datasets/{source}/rename"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({"name":target}).to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn rename_preserves_identity_and_data_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = st.create("old", DbType::Persistent).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<urn:s> <urn:p> 1 .".to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let id = ds.store.dataset_id();
    let app = router(st.clone());
    assert_eq!(
        rename(&app, "old", "new").await.status,
        StatusCode::CONFLICT
    );
    assert!(st.get("old").is_some());
    drop(ds);
    let r = rename(&app, "old", "new").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["renamedFrom"], "old");
    assert!(st.get("old").is_none());
    assert_eq!(st.get("new").unwrap().store.dataset_id(), id);
    assert_eq!(st.get("new").unwrap().dataset.len(), 1);
    drop(app);
    drop(st);
    let reopened =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    assert!(reopened.get("old").is_none());
    assert_eq!(reopened.get("new").unwrap().store.dataset_id(), id);
}

#[tokio::test]
async fn rename_rejects_invalid_missing_existing_reserved_and_live_branch() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    drop(st.create("old", DbType::Persistent).unwrap());
    drop(st.create("existing", DbType::Persistent).unwrap());
    let app = router(st.clone());
    assert_eq!(
        rename(&app, "old", "../bad").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        rename(&app, "missing", "new").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rename(&app, "old", "existing").await.status,
        StatusCode::CONFLICT
    );
    let reservation = st
        .catalog
        .reserve("old", sparkles::catalog::ReservationKind::Restore, "test")
        .unwrap();
    assert_eq!(
        rename(&app, "old", "new").await.status,
        StatusCode::CONFLICT
    );
    drop(reservation);
    let main = st.get("old").unwrap();
    main.store
        .create_branch("work", &Default::default())
        .unwrap();
    let branch = main.dataset.branch("work").unwrap();
    drop(main);
    assert_eq!(
        rename(&app, "old", "new").await.status,
        StatusCode::CONFLICT
    );
    drop(branch);
    assert_eq!(rename(&app, "old", "new").await.status, StatusCode::OK);
}

#[tokio::test]
async fn rename_read_only_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    drop(st.create("old", DbType::Mem).unwrap());
    st.read_only = true;
    assert_eq!(
        rename(&router(Arc::new(st)), "old", "new").await.status,
        StatusCode::FORBIDDEN
    );
}
