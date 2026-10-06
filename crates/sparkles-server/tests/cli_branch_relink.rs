//! Offline branch relinking through the actual CLI and fresh database opens.

use serde_json::Value;
use sparkles::history::At;
use sparkles::store::{Snapshot, Store};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("SPARKLES_SERVER")
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let output = run(dir, args);
    assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    output
}

fn state(snapshot: &Arc<Snapshot>) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    snapshot
        .for_each_quad(|q| {
            output.insert(sparkles::annotations::nquads_line(
                &snapshot.quad_to_terms(q).unwrap(),
            ));
            Ok(())
        })
        .unwrap();
    output
}

fn update(store: &Store, text: &str) {
    sparkles::sparql::update::update(store, text, &Default::default()).unwrap();
}

fn fixture(dir: &Path) -> Store {
    let store = Store::open(&dir.join("db"), Default::default()).unwrap();
    update(
        &store,
        "INSERT DATA { _:base <urn:p> 1 . <urn:shared> <urn:p> 2 }",
    );
    store.compact().unwrap();
    store.create_branch("dev", &Default::default()).unwrap();
    let branch = store.branch("dev").unwrap();
    update(
        &branch,
        "DELETE DATA { <urn:shared> <urn:p> 2 }; INSERT DATA { _:branch <urn:p> 3 }",
    );
    branch.create_snapshot("before", &At::Head, None).unwrap();
    update(&branch, "INSERT DATA { <urn:dev-only> <urn:p> 4 }");
    update(
        &store,
        "DELETE DATA { <urn:shared> <urn:p> 2 }; INSERT DATA { <urn:main-only> <urn:p> 5 }",
    );
    store.compact().unwrap();
    drop(branch);
    store
}

#[test]
fn relink_preserves_reopened_identity_state_blank_nodes_and_pin() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let branch = store.branch("dev").unwrap();
    let before = state(&branch.snapshot());
    let pinned = state(
        &branch
            .snapshot_at(&At::Snapshot("before".into()), &Default::default())
            .unwrap()
            .0,
    );
    let identity = (
        branch.dataset_id(),
        branch.head_commit().seq,
        branch.branch_ordinal(),
    );
    let main = state(&store.snapshot());
    let shared_generation = store.snapshot().generation.name.clone();
    let previous_generation = branch.snapshot().generation.name.clone();
    store.set_branch_protected("dev", true).unwrap();
    drop(branch);
    drop(store);

    let output = ok(
        dir.path(),
        &["branch", "relink", "--loc", "db", "dev", "--format", "json"],
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["mode"], "relink");
    assert!(report["generation"].as_str().unwrap() != previous_generation);
    assert_eq!(report["quads"], before.len());
    assert!(stderr(&output).contains("relinking branch"));
    assert!(stderr(&output).contains("relinked branch"));

    let store = Store::open(&dir.path().join("db"), Default::default()).unwrap();
    let branch = store.branch("dev").unwrap();
    assert_eq!(
        (
            branch.dataset_id(),
            branch.head_commit().seq,
            branch.branch_ordinal()
        ),
        identity
    );
    assert_eq!(state(&branch.snapshot()), before);
    assert_eq!(state(&store.snapshot()), main);
    assert!(store.branch_info("dev").unwrap().protected);
    assert!(
        branch
            .snapshot()
            .generation
            .linked()
            .unwrap()
            .base_dir()
            .ends_with(&shared_generation)
    );
    assert_eq!(
        state(
            &branch
                .snapshot_at(&At::Snapshot("before".into()), &Default::default())
                .unwrap()
                .0
        ),
        pinned
    );
    // Reopening the allocator must not reuse an inherited or overlay blank identity.
    store.set_branch_protected("dev", false).unwrap();
    update(&branch, "INSERT DATA { _:fresh <urn:p> 6 }");
    let after = state(&branch.snapshot());
    assert!(before.is_subset(&after));
    let blanks = |rows: &BTreeSet<String>| {
        rows.iter()
            .filter(|q| q.starts_with("_:"))
            .map(|q| q.split_whitespace().next().unwrap().to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(blanks(&after).len(), blanks(&before).len() + 1);
    drop(branch);
    drop(store);
    let output = ok(dir.path(), &["branch", "relink", "--loc", "db", "dev"]);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("relinked branch dev into "));
    let store = Store::open(&dir.path().join("db"), Default::default()).unwrap();
    assert_eq!(state(&store.branch("dev").unwrap().snapshot()), after);
}

#[test]
fn refusals_leave_existing_state_and_missing_paths_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let branch = store.branch("dev").unwrap();
    branch.compact().unwrap();
    let before = state(&branch.snapshot());
    let identity = (branch.dataset_id(), branch.head_commit().seq);
    let current = branch.snapshot().generation.name.clone();
    drop(branch);
    drop(store);
    for (args, message) in [
        (
            vec!["branch", "relink", "--loc", "db", "main"],
            "main cannot be relinked",
        ),
        (
            vec!["branch", "relink", "--loc", "db", "dev"],
            "already owns its index",
        ),
        (
            vec!["branch", "relink", "--loc", "db", "absent"],
            "no such branch",
        ),
        (
            vec!["branch", "relink", "--loc", "missing", "dev"],
            "no CURRENT",
        ),
        (
            vec!["branch", "relink", "--loc", "memory://", "dev"],
            "no CURRENT",
        ),
        (
            vec!["branch", "relink", "--loc", "db", "dev", "--branch", "dev"],
            "does not take --branch",
        ),
    ] {
        let output = run(dir.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(message),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    for args in [
        vec!["branch", "relink", "--server", "http://127.0.0.1:1", "dev"],
        vec![
            "branch", "relink", "--loc", "missing", "dev", "--format", "yaml",
        ],
        vec!["branch", "relink", "dev"],
    ] {
        assert_eq!(run(dir.path(), &args).status.code(), Some(2), "{args:?}");
    }
    assert!(!dir.path().join("missing").exists());
    assert!(!dir.path().join("memory:").exists());
    let store = Store::open(&dir.path().join("db"), Default::default()).unwrap();
    let branch = store.branch("dev").unwrap();
    assert_eq!((branch.dataset_id(), branch.head_commit().seq), identity);
    assert_eq!(branch.snapshot().generation.name, current);
    assert_eq!(state(&branch.snapshot()), before);
}

#[test]
fn active_catalog_dataset_lock_refuses_offline_relink() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let dataset = catalog.create("wiki", &Default::default()).unwrap();
    dataset.create_branch("dev", &Default::default()).unwrap();
    let branch = dataset.branch("dev").unwrap();
    let identity = (branch.dataset_id(), branch.head_commit().seq);
    let output = run(
        dir.path(),
        &["branch", "relink", "--loc", "data/databases/wiki", "dev"],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("in use by another process"),
        "{}",
        stderr(&output)
    );
    assert_eq!((branch.dataset_id(), branch.head_commit().seq), identity);
    branch
        .update("INSERT DATA { <urn:still-writable> <urn:p> 1 }")
        .unwrap();
}

#[cfg(feature = "backup")]
#[test]
fn relinked_branch_backup_restores_without_shared_index_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let branch = store.branch("dev").unwrap();
    let identity = branch.dataset_id();
    let expected = state(&branch.snapshot());
    drop(branch);
    drop(store);
    let repo = format!("file://{}", dir.path().join("repo").display());
    ok(
        dir.path(),
        &[
            "backup",
            "create",
            "--loc",
            "db",
            "--branch",
            "dev",
            "--repo",
            &repo,
            "--name",
            "before-relink",
        ],
    );
    ok(dir.path(), &["branch", "relink", "--loc", "db", "dev"]);
    ok(
        dir.path(),
        &[
            "backup", "create", "--loc", "db", "--branch", "dev", "--repo", &repo, "--name", "dev",
        ],
    );
    // Remove the source dataset entirely: restore must use the repository alone.
    std::fs::remove_dir_all(dir.path().join("db")).unwrap();
    for (name, destination) in [("before-relink", "before"), ("dev", "copy")] {
        ok(
            dir.path(),
            &[
                "backup",
                "restore",
                name,
                "--repo",
                &repo,
                "--to",
                destination,
            ],
        );
        let restored = Store::open(&dir.path().join(destination), Default::default()).unwrap();
        assert_ne!(restored.dataset_id(), identity);
        assert_eq!(restored.forked_from().unwrap().id, identity);
        assert_eq!(state(&restored.snapshot()), expected);
        assert_eq!(restored.branches().unwrap().len(), 1);
        assert!(restored.snapshot().generation.linked().is_none());
    }
}
