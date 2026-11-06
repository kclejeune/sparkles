//! Cached branch identity while a rename waits for a transaction worker.

use sparkles::{Dataset, Error};
use std::time::{Duration, Instant};

fn rename_case(storage: &str) {
    let root = std::path::PathBuf::from(std::env::var_os("SPARKLES_BRANCH_CACHE_ROOT").unwrap());
    let main = if storage == "memory" {
        Dataset::memory()
    } else {
        Dataset::open(root).unwrap()
    };
    let info = main.create_branch("work", &Default::default()).unwrap();
    main.set_branch_note("work", Some("original".into()))
        .unwrap();
    let work = main.branch("work").unwrap();
    let held = work.store().write();
    std::thread::scope(|scope| {
        let rename = scope.spawn(|| main.rename_branch("work", "renamed"));
        // This reads only the table. The rename cannot rekey Dataset's cache
        // until its returned BranchInfo obtains the writer held above.
        let deadline = Instant::now() + Duration::from_secs(3);
        let published = loop {
            if main.store().branch_id_of("renamed").ok() == Some(info.id) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let cached = published.then(|| main.cached_branch("renamed"));
        let alias = published.then(|| main.branch("renamed"));
        let refusal = published.then(|| main.set_branch_note("renamed", Some("refused".into())));
        // Release before assertions and the scoped join. The outer subprocess
        // deadline bounds a broken cache lookup that waits before reaching here.
        drop(held);
        let renamed = rename.join().unwrap().unwrap();
        assert!(published, "rename did not publish its table entry");
        assert_eq!(renamed.id, info.id);
        let cached = cached.unwrap().unwrap().expect("renamed cached identity");
        assert!(std::ptr::eq(cached.state(), work.state()));
        let alias = alias
            .unwrap()
            .expect("listed identity must find its live cached handle");
        assert!(std::ptr::eq(alias.state(), work.state()));
        assert!(matches!(refusal.unwrap(), Err(Error::Conflict(_))));
    });
    assert_eq!(
        main.branch_info("renamed").unwrap().note.as_deref(),
        Some("original")
    );
    main.set_branch_note("renamed", Some("after release".into()))
        .unwrap();
    // Store-level creation leaves no Dataset handle in the host's cache. A
    // metadata ownership check must keep it cold until the operation needs it.
    main.store()
        .create_branch("cold", &Default::default())
        .unwrap();
    assert!(main.cached_branch("cold").unwrap().is_none());
    let cold = main.branch("cold").unwrap();
    assert!(std::ptr::eq(
        main.cached_branch("cold").unwrap().unwrap().state(),
        cold.state(),
    ));
    assert!(std::ptr::eq(
        main.cached_branch("main").unwrap().unwrap().state(),
        main.state(),
    ));
}

#[test]
fn cached_branch_follows_identity_during_rename() {
    if let Ok(storage) = std::env::var("SPARKLES_BRANCH_CACHE_CHILD") {
        rename_case(&storage);
        return;
    }
    let mut failed = Vec::new();
    for storage in ["memory", "persistent"] {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cached_branch_follows_identity_during_rename",
                "--nocapture",
            ])
            .env("SPARKLES_BRANCH_CACHE_CHILD", storage)
            .env("SPARKLES_BRANCH_CACHE_ROOT", root.path().join("db"))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                if !status.success() {
                    failed.push(storage);
                }
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                failed.push(storage);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        failed.is_empty(),
        "cached identity lookup failed: {failed:?}"
    );
}
