//! The repository tests against a real S3-compatible service (MinIO), when
//! `SPARKLES_TEST_S3_ENDPOINT` is set (skipped otherwise). The bucket
//! (`SPARKLES_TEST_S3_BUCKET`, default `sparkles-test`) must exist, and credentials
//! come from the usual `AWS_*` variables. Each run uses a fresh prefix.

mod common;

use common::*;
use object_store::ObjectStoreExt;
use sparkles_backup::{
    Code, ListFilter, OpenEnv, RepoConfig, Repository, VerifyLevel, VerifyOptions, VerifyStatus,
};

fn s3_config(name: &str, prefix: &str) -> Option<RepoConfig> {
    let endpoint = std::env::var("SPARKLES_TEST_S3_ENDPOINT").ok()?;
    let bucket =
        std::env::var("SPARKLES_TEST_S3_BUCKET").unwrap_or_else(|_| "sparkles-test".into());
    let endpoint: String =
        percent_encoding::utf8_percent_encode(&endpoint, percent_encoding::NON_ALPHANUMERIC)
            .collect();
    let url = format!(
        "s3://{bucket}/{prefix}?endpoint={endpoint}&allow_http=true&path_style=true&region=us-east-1"
    );
    Some(RepoConfig::from_url(name, &url).unwrap())
}

#[tokio::test]
async fn s3_semantics() {
    let prefix = uuid::Uuid::new_v4().to_string();
    let Some(cfg) = s3_config("s3", &prefix) else {
        eprintln!("SPARKLES_TEST_S3_ENDPOINT is not set: skipped");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    make_db(&a);
    make_db(&b);
    let repo = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    let t = repo.test().await.unwrap();
    assert!(t.ok && t.conditional_writes, "{t:?}");

    // a first and an incremental backup
    repo.create(closed_source(&a), &opts("b1", "a"))
        .await
        .unwrap();
    commit(&a, "INSERT DATA { <urn:c> <urn:p> 3 }");
    let s2 = repo
        .create(closed_source(&a), &opts("b2", "a"))
        .await
        .unwrap();
    assert_eq!(s2.commit.seq, 4);
    let m2 = repo.manifest("b2").await.unwrap();
    assert!(
        m2.stats.new_blobs <= 5 && m2.stats.added_bytes < 10_000,
        "{:?}",
        m2.stats
    );

    // a second handle racing for a taken name
    let other = Repository::open(&cfg, &OpenEnv::default()).await.unwrap();
    assert_eq!(other.id(), repo.id());
    let e = other
        .create(closed_source(&b), &opts("b2", "b"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::BackupExists);

    // verification finds a missing blob
    let v = repo
        .verify(
            &[],
            &VerifyOptions {
                level: VerifyLevel::Data,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(v.status, VerifyStatus::Ok, "{v:?}");
    let id = m2
        .files
        .iter()
        .find(|f| f.path == "gen-0001/spo.dat")
        .unwrap()
        .blobs[0]
        .id
        .clone();
    repo.store()
        .delete(&sparkles_backup::layout::blob_key(&id))
        .await
        .unwrap();
    let v = repo
        .verify(&["b2".to_string()], &VerifyOptions::default())
        .await
        .unwrap();
    assert_eq!(v.backups[0].missing, [id]);

    // deleting a backup keeps its blobs
    assert!(repo.delete("b1").await.unwrap());
    let names: Vec<String> = repo
        .list(&ListFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["b2"]);
}
