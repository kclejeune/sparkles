#![cfg(feature = "backup")]
use sparkles::backup::{self, CreateOptions, Identity, RepoConfig, RepoType, RestoreRequest};
use sparkles::{Catalog, Error};

fn config(name: &str, path: &std::path::Path) -> RepoConfig {
    RepoConfig {
        name: name.into(),
        kind: RepoType::Fs,
        path: Some(path.display().to_string()),
        ..Default::default()
    }
}
fn create_options(name: &str) -> CreateOptions {
    CreateOptions {
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn repositories_persist_and_fixed_entries_are_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    let cfg = config("local", &dir.path().join("repo"));
    repos.add(cfg.clone()).unwrap();
    assert!(repos.add(cfg.clone()).is_err());
    let id = repos.open("local").unwrap().id();
    let mut changed = cfg;
    changed.readonly = true;
    repos.update("local", changed).unwrap();
    assert!(repos.get("local").unwrap().config.readonly);
    let fixed = config("fixed", &dir.path().join("fixed"));
    repos
        .clone()
        .with_fixed(std::slice::from_ref(&fixed))
        .unwrap();
    assert!(repos.remove("fixed").is_err());
    assert!(repos.update("fixed", fixed).is_err());
    assert!(repos.get("fixed").is_ok());
    drop(repos);
    drop(cat);
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    assert!(repos.get("fixed").is_err());
    assert_eq!(repos.open("local").unwrap().id(), id);
    assert!(repos.remove("local").unwrap());
}

#[test]
fn backup_restore_identity_and_omitted_branches() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    repos
        .add(config("local", &dir.path().join("repo")))
        .unwrap();
    let repo = repos.open("local").unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    ds.create_branch("dev", &Default::default()).unwrap();
    ds.backups(&repo)
        .create_with(&create_options("first"), &Default::default())
        .unwrap();
    assert_eq!(
        ds.backups(&repo)
            .get("first")
            .unwrap()
            .unwrap()
            .branches_omitted,
        1
    );
    let copy = cat
        .restore(
            &repo,
            "first",
            &RestoreRequest {
                target: Some("copy".into()),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert_eq!(copy.len(), 1);
    assert_ne!(copy.dataset_id(), ds.dataset_id());
    assert_eq!(copy.branches().unwrap().len(), 1);
    assert!(
        cat.restore(
            &repo,
            "first",
            &RestoreRequest {
                target: Some("duplicate".into()),
                identity: Identity::Keep,
                ..Default::default()
            },
            &Default::default()
        )
        .is_err()
    );
    assert!(cat.get("duplicate").is_none());
    drop(copy);
    drop(ds);
    let restored = cat
        .restore(
            &repo,
            "first",
            &RestoreRequest {
                target: Some("wiki".into()),
                replace: true,
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert_eq!(restored.len(), 1);
    assert!(cat.restoring_by("wiki").is_none());
}

#[test]
fn cancelled_backups_and_restores_publish_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    repos
        .add(config("local", &dir.path().join("repo")))
        .unwrap();
    let repo = repos.open("local").unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.backups(&repo)
        .create_with(&create_options("first"), &Default::default())
        .unwrap();
    let ctl = sparkles::task::Control::none();
    ctl.cancel.cancel();
    assert!(matches!(
        ds.backups(&repo)
            .create_with(&create_options("cancelled"), &ctl),
        Err(Error::Cancelled)
    ));
    assert!(ds.backups(&repo).get("cancelled").unwrap().is_none());
    assert!(matches!(
        cat.restore(
            &repo,
            "first",
            &RestoreRequest {
                target: Some("copy".into()),
                ..Default::default()
            },
            &ctl
        ),
        Err(Error::Cancelled)
    ));
    assert!(cat.get("copy").is_none());
    assert!(cat.reserved_by("copy").is_none());
}

#[test]
fn manual_policy_runs_skip_unchanged_and_apply_retention() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    cat.repositories()
        .unwrap()
        .add(config("local", &dir.path().join("repo")))
        .unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    let mut policy: backup::PolicyConfig = serde_json::from_value(serde_json::json!({
        "name":"nightly", "repository":"local", "datasets":["wi*"], "schedule":"every 1h",
        "skipUnchanged":true, "nameTemplate":"{dataset}-{seq}", "retention":{"maxCount":1,"minCount":1}
    }))
    .unwrap();
    let first = cat.run_policy(&policy, &Default::default()).unwrap();
    assert_eq!(first.datasets[0].result, backup::DatasetRunResult::Ok);
    assert_eq!(
        cat.run_policy(&policy, &Default::default())
            .unwrap()
            .datasets[0]
            .reason
            .as_deref(),
        Some("unchanged")
    );
    ds.update("INSERT DATA { <urn:t> <urn:p> 2 }").unwrap();
    policy.retention.min_count = 1;
    cat.run_policy(&policy, &Default::default()).unwrap();
    let repo = cat.repositories().unwrap().open("local").unwrap();
    assert_eq!(
        backup::blocking(&repo)
            .list(&Default::default())
            .unwrap()
            .len(),
        1
    );
    assert!(cat.apply_retention(&policy, true).unwrap().dry_run);
}

#[test]
fn selected_branch_backups_restore_as_independent_main_datasets() {
    for memory in [false, true] {
        for compact in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
            cat.repositories()
                .unwrap()
                .add(config("local", &dir.path().join("repo")))
                .unwrap();
            let repo = cat.repositories().unwrap().open("local").unwrap();
            let opts = sparkles::catalog::CreateDataset {
                kind: if memory {
                    sparkles::catalog::DatasetKind::Memory
                } else {
                    sparkles::catalog::DatasetKind::Persistent
                },
                ..Default::default()
            };
            let ds = cat.create("wiki", &opts).unwrap();
            ds.update("INSERT DATA { _:main <urn:p> 1 }").unwrap();
            ds.create_branch("dev", &Default::default()).unwrap();
            let dev = ds.branch("dev").unwrap();
            dev.update("INSERT DATA { _:dev <urn:p> 2 }").unwrap();
            if compact {
                dev.store().compact().unwrap();
            }
            let summary = dev
                .backups(&repo)
                .create_with(&create_options("branch"), &Default::default())
                .unwrap();
            let branch = summary.dataset.branch.as_ref().unwrap();
            assert_eq!(branch.dataset_id, ds.dataset_id());
            assert_eq!(branch.id, dev.dataset_id());
            assert_eq!(branch.name, "dev");
            assert_eq!(
                summary.dataset.kind,
                if memory { "mem" } else { "persistent" }
            );
            let restored = cat
                .restore(
                    &repo,
                    "branch",
                    &RestoreRequest {
                        target: Some("copy".into()),
                        ..Default::default()
                    },
                    &Default::default(),
                )
                .unwrap();
            assert_eq!(restored.len(), 2);
            assert_ne!(restored.dataset_id(), dev.dataset_id());
            assert_eq!(restored.store().forked_from().unwrap().id, dev.dataset_id());
            assert_eq!(restored.branches().unwrap().len(), 1);
            restored.create_branch("new", &Default::default()).unwrap();
            assert!(restored.branch_info("new").unwrap().ordinal >= branch.next_ordinal as u16);
            assert!(
                cat.restore(
                    &repo,
                    "branch",
                    &RestoreRequest {
                        target: Some("keep".into()),
                        identity: Identity::Keep,
                        ..Default::default()
                    },
                    &Default::default()
                )
                .is_err()
            );
            assert!(cat.get("keep").is_none());
            assert_eq!(ds.len(), 1);
            assert_eq!(dev.len(), 2);
        }
    }
}

#[cfg(feature = "shex")]
#[test]
fn memory_branch_backup_preserves_graph_sourced_shex_guard() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    cat.repositories()
        .unwrap()
        .add(config("local", &dir.path().join("repo")))
        .unwrap();
    let repo = cat.repositories().unwrap().open("local").unwrap();
    let ds = cat
        .create(
            "wiki",
            &sparkles::catalog::CreateDataset {
                kind: sparkles::catalog::DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    let schema =
        sparkles_shex::Schema::parse_shexc("<urn:S> { a [<urn:T>] ; <urn:p> . }", None).unwrap();
    let graph = sparkles_shex::shexr::to_graph(&schema);
    let triples = graph
        .iter()
        .map(|t| format!("{t} ."))
        .collect::<Vec<_>>()
        .join("\n");
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <urn:schema> {{ {triples} }} <urn:n> a <urn:T>; <urn:p> 1 }}"
    ))
    .unwrap();
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(serde_json::json!({
                "language":"shex", "mode":"reject", "schema":{"graphs":["urn:schema"]},
                "shapeMap":"{ FOCUS a <urn:T> }@<urn:S>"
            }))
            .unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let work = ds.branch("work").unwrap();
    assert!(work.update("INSERT DATA { <urn:bad> a <urn:T> }").is_err());
    work.backups(&repo)
        .create_with(&create_options("guarded"), &Default::default())
        .unwrap();
    let restored = cat
        .restore(
            &repo,
            "guarded",
            &RestoreRequest {
                target: Some("copy".into()),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert!(restored.write_guard().is_some());
    assert!(
        restored
            .update("INSERT DATA { <urn:bad> a <urn:T> }")
            .is_err()
    );
    assert!(!restored.ask("ASK { <urn:bad> a <urn:T> }").unwrap());
}

#[test]
fn main_backups_reserve_blank_node_ordinals_after_merged_branches_are_deleted() {
    for memory in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
        cat.repositories()
            .unwrap()
            .add(config("local", &dir.path().join("repo")))
            .unwrap();
        let repo = cat.repositories().unwrap().open("local").unwrap();
        let ds = cat
            .create(
                "wiki",
                &sparkles::catalog::CreateDataset {
                    kind: if memory {
                        sparkles::catalog::DatasetKind::Memory
                    } else {
                        sparkles::catalog::DatasetKind::Persistent
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        ds.create_branch("work", &Default::default()).unwrap();
        let work = ds.branch("work").unwrap();
        work.update("INSERT DATA { _:original <urn:p> 1 }").unwrap();
        ds.merge("work", "main", &Default::default()).unwrap();
        ds.delete_branch("work", true).unwrap();
        let summary = ds
            .backups(&repo)
            .create_with(&create_options("main"), &Default::default())
            .unwrap();
        assert!(summary.dataset.branch.is_none());
        let restored = cat
            .restore(
                &repo,
                "main",
                &RestoreRequest {
                    target: Some("copy".into()),
                    ..Default::default()
                },
                &Default::default(),
            )
            .unwrap();
        assert_eq!(restored.branches().unwrap().len(), 1);
        let new = restored.create_branch("new", &Default::default()).unwrap();
        assert_eq!(new.ordinal, 2);
        let branch = restored.branch("new").unwrap();
        branch
            .update("INSERT DATA { _:independent <urn:p> 2 }")
            .unwrap();
        assert!(
            branch
                .ask("ASK { ?a <urn:p> 1 . ?b <urn:p> 2 . FILTER(?a != ?b) }")
                .unwrap()
        );
    }
}
