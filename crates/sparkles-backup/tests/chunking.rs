mod common;

use common::*;
use object_store::{ObjectStoreExt, memory::InMemory};
use sparkles_backup::{Code, FileKind, OpenEnv, Repository, RepositoryOptions, layout};
use std::sync::Arc;

#[tokio::test]
async fn opt_in_chunking_reopens_and_resynchronizes_after_an_insertion() {
    let mem = Arc::new(InMemory::new());
    let cfg = memory_config("chunked");
    let env = OpenEnv {
        store: Some(mem.clone()),
        ..Default::default()
    };
    let options = RepositoryOptions {
        content_defined_chunking: true,
        ..Default::default()
    };
    let data = noise(24 << 20, 141);
    let mut shifted = data[..590_000].to_vec();
    shifted.extend_from_slice(b"inserted term");
    shifted.extend_from_slice(&data[590_000..]);
    let first = Repository::open_with_options(&cfg, &env, &options)
        .await
        .unwrap();
    assert_eq!(first.marker().format, 2);
    let path = "gen-0001/vocab.dat";
    let snapshot = |data| {
        synthetic(vec![
            ("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec()),
            (path, FileKind::Immutable, data),
        ])
    };
    first
        .create(snapshot(data.clone()), &opts("first", "ds"))
        .await
        .unwrap();
    let reopened = Repository::open(&cfg, &env).await.unwrap();
    reopened
        .create(snapshot(shifted), &opts("second", "ds"))
        .await
        .unwrap();
    let get_vocab =
        |m: sparkles_backup::Manifest| m.files.into_iter().find(|f| f.path == path).unwrap();
    let first = get_vocab(reopened.manifest("first").await.unwrap());
    let second_manifest = reopened.manifest("second").await.unwrap();
    assert!(
        second_manifest
            .requires
            .iter()
            .any(|f| f == "fastcdc-2016/1")
    );
    let second = get_vocab(second_manifest);
    assert!(first.chunker.is_some());
    assert_ne!(first.blobs[0], second.blobs[0]);
    assert!(
        second
            .blobs
            .iter()
            .skip(1)
            .any(|blob| first.blobs.contains(blob))
    );
    let (fixed, _) = memory_repo().await;
    assert_eq!(fixed.marker().format, 1);
    fixed
        .create(snapshot(data), &opts("fixed", "ds"))
        .await
        .unwrap();
    let fixed_vocab = get_vocab(fixed.manifest("fixed").await.unwrap());
    assert_eq!(fixed_vocab.blobs.len(), 1);
    assert!(fixed_vocab.chunker.is_none());

    let mut unknown = serde_json::to_value(reopened.marker()).unwrap();
    unknown["format"] = 3.into();
    mem.put(
        &layout::marker_key(),
        serde_json::to_vec(&unknown).unwrap().into(),
    )
    .await
    .unwrap();
    assert_eq!(
        Repository::open(&cfg, &env).await.unwrap_err().code(),
        Code::IncompatibleRepository
    );
}

#[cfg(feature = "fs")]
#[tokio::test]
async fn copies_and_restores_a_chunked_backup_in_both_repository_formats() {
    use sparkles_backup::{CopyOptions, RestoreOptions};

    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("db");
    make_db(&db);
    let options = RepositoryOptions {
        content_defined_chunking: true,
        ..Default::default()
    };
    let env = OpenEnv::default();
    let source = Repository::open_with_options(&memory_config("source"), &env, &options)
        .await
        .unwrap();
    let dest = Repository::open_with_options(&memory_config("target"), &env, &options)
        .await
        .unwrap();
    source
        .create(closed_source(&db), &opts("chunked", "ds"))
        .await
        .unwrap();
    let (fixed, _) = memory_repo().await;
    source
        .copy_to(&fixed, &CopyOptions::default())
        .await
        .unwrap();
    let fixed_manifest = fixed.manifest("chunked").await.unwrap();
    assert!(
        fixed_manifest
            .files
            .iter()
            .all(|file| file.chunker.is_none())
    );
    assert!(
        !fixed_manifest
            .requires
            .iter()
            .any(|feature| feature == "fastcdc-2016/1")
    );
    let fixed_restore = temp.path().join("fixed-restore");
    fixed
        .restore("chunked", &fixed_restore, &RestoreOptions::default())
        .await
        .unwrap();
    let retry = source
        .copy_to(&fixed, &CopyOptions::default())
        .await
        .unwrap();
    assert_eq!(retry.already_present, ["chunked"]);
    source
        .copy_to(&dest, &CopyOptions::default())
        .await
        .unwrap();
    let staged = temp.path().join("restore");
    dest.restore("chunked", &staged, &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(staged.join("gen-0001/vocab.dat")).unwrap(),
        std::fs::read(db.join("gen-0001/vocab.dat")).unwrap()
    );
}

#[tokio::test]
async fn copy_uses_destination_boundaries_and_keeps_empty_vocabulary_readable() {
    use sparkles_backup::CopyOptions;
    let data = noise(24 << 20, 42);
    let path = "gen-0001/vocab.dat";
    let snapshot = |bytes| {
        synthetic(vec![
            ("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec()),
            (path, FileKind::Immutable, bytes),
        ])
    };
    let (fixed, _) = memory_repo().await;
    let options = RepositoryOptions {
        content_defined_chunking: true,
        ..Default::default()
    };
    let dest = Repository::open_with_options(&memory_config("cdc"), &OpenEnv::default(), &options)
        .await
        .unwrap();
    fixed
        .create(snapshot(data.clone()), &opts("copied", "ds"))
        .await
        .unwrap();
    fixed
        .create(snapshot(Vec::new()), &opts("empty", "ds"))
        .await
        .unwrap();
    fixed.copy_to(&dest, &CopyOptions::default()).await.unwrap();
    dest.create(snapshot(data), &opts("native", "ds"))
        .await
        .unwrap();
    let vocab =
        |m: sparkles_backup::Manifest| m.files.into_iter().find(|file| file.path == path).unwrap();
    let copied = vocab(dest.manifest("copied").await.unwrap());
    let native = vocab(dest.manifest("native").await.unwrap());
    assert_eq!(copied.blobs, native.blobs);
    let empty = vocab(dest.manifest("empty").await.unwrap());
    assert_eq!(empty.blobs.len(), 1);
    assert_eq!(empty.blobs[0].size, 0);
    let retry = fixed.copy_to(&dest, &CopyOptions::default()).await.unwrap();
    assert_eq!(retry.already_present.len(), 2);
}
