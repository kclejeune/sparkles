//! Backups, restores and clones carry the views' definitions (roadmap item 220, spec F05
//! "Recovery of managed definitions"). Restored and cloned views come back paused until
//! an operator resumes them.
#![cfg(feature = "backup")]
use serde_json::Value as J;
use sparkles::backup::{CreateOptions, RepoConfig, RepoType, RestoreRequest};
use sparkles::catalog::{CloneRequest, CreateDataset, DatasetKind};
use sparkles::incr::{ViewDef, ViewState};
use sparkles::{Catalog, Dataset, Error};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SIZES: &str = "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p <urn:in> ?t } GROUP BY ?t";
const MEMBERS: &str = "SELECT ?p WHERE { ?p <urn:in> <urn:t1> }";

struct Fixture {
    _dir: tempfile::TempDir,
    data: PathBuf,
    repo_dir: PathBuf,
    cat: Catalog,
    repo: std::sync::Arc<sparkles::backup::Repository>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let repo_dir = dir.path().join("repo");
    let cat = Catalog::open(&data, Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    repos
        .add(RepoConfig {
            name: "local".into(),
            kind: RepoType::Fs,
            path: Some(repo_dir.display().to_string()),
            ..Default::default()
        })
        .unwrap();
    let repo = repos.open("local").unwrap();
    Fixture {
        _dir: dir,
        data,
        repo_dir,
        cat,
        repo,
    }
}

fn with_views(ds: &Dataset) {
    ds.update(
        "INSERT DATA { <urn:a> <urn:in> <urn:t1> . <urn:b> <urn:in> <urn:t1> . <urn:c> <urn:in> <urn:t2> }",
    )
    .unwrap();
    ds.views().put("sizes", ViewDef::table(SIZES)).unwrap();
    ds.views().put("members", ViewDef::table(MEMBERS)).unwrap();
    for v in ["sizes", "members"] {
        ds.views().wait_built(v, Duration::from_secs(10)).unwrap();
    }
}

fn backup(f: &Fixture, ds: &Dataset, name: &str) {
    ds.backups(&f.repo)
        .create_with(
            &CreateOptions {
                name: name.into(),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
}

fn restore(f: &Fixture, name: &str, target: &str) -> sparkles::Result<Dataset> {
    f.cat.restore(
        &f.repo,
        name,
        &RestoreRequest {
            target: Some(target.into()),
            ..Default::default()
        },
        &Default::default(),
    )
}

fn rows(ds: &Dataset, view: &str) -> Vec<String> {
    let mut v: Vec<String> = ds
        .views()
        .read(view)
        .unwrap()
        .rows
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    v.sort();
    v
}

fn definitions(ds: &Dataset) -> Vec<(String, ViewDef)> {
    ds.views()
        .list()
        .into_iter()
        .map(|v| (v.name, v.definition))
        .collect()
}

fn assert_paused(ds: &Dataset, reason: &str) {
    let list = ds.views().list();
    assert!(!list.is_empty());
    for v in list {
        assert_eq!(
            v.state,
            ViewState::Suspended {
                reason: reason.into()
            },
            "{}",
            v.name
        );
    }
}

/// Every view comes back with its definition, paused, and answers the same rows once
/// resumed. The pause survives a restart until the view is resumed.
#[test]
fn views_round_trip_paused_until_resumed() {
    let f = fixture();
    let ds = f.cat.create("wiki", &Default::default()).unwrap();
    with_views(&ds);
    backup(&f, &ds, "b1");
    let m = ds.backups(&f.repo).get("b1").unwrap().unwrap();
    assert_eq!(m.format, 1);
    assert_eq!(m.requires, ["definitions"]);
    let defs: Vec<_> = m.files.iter().filter(|f| f.definition.is_some()).collect();
    assert_eq!(defs.len(), 2);
    assert!(defs.iter().all(|d| d.path.starts_with("views/")));

    let copy = restore(&f, "b1", "copy").unwrap();
    assert_eq!(definitions(&copy), definitions(&ds));
    assert_paused(&copy, "restored");
    let e = copy.views().read("sizes").err().unwrap();
    assert!(e.to_string().contains("suspended (restored)"), "{e}");
    let want = [rows(&ds, "sizes"), rows(&ds, "members")];
    drop(copy);
    drop(ds);
    // a restart keeps the views paused
    let Fixture {
        _dir,
        data,
        cat,
        repo,
        ..
    } = f;
    drop(repo);
    drop(cat);
    let cat = Catalog::open(&data, Default::default()).unwrap();
    let copy = cat.get("copy").unwrap();
    assert_paused(&copy, "restored");
    let marker = data.join("databases/copy/views/sizes/paused.json");
    assert!(marker.is_file());
    for (v, want) in ["sizes", "members"].into_iter().zip(want) {
        copy.views().rebuild(v).unwrap();
        copy.views().wait_built(v, Duration::from_secs(10)).unwrap();
        assert_eq!(rows(&copy, v), want, "{v}");
    }
    assert!(!marker.exists());
    assert!(
        copy.views()
            .list()
            .iter()
            .all(|v| v.state == ViewState::Current),
        "{:?}",
        copy.views().list()
    );
}

/// A backup without views has the layout older releases wrote and read, and a backup
/// of an in-memory dataset carries its views too.
#[test]
fn backups_without_views_keep_the_older_layout_and_memory_views_restore() {
    let f = fixture();
    let ds = f.cat.create("plain", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    backup(&f, &ds, "plain");
    let raw = manifest_json(&f, "plain");
    assert!(raw.get("requires").is_none(), "{raw}");
    assert!(
        raw["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f.get("definition").is_none())
    );
    let copy = restore(&f, "plain", "plain-copy").unwrap();
    assert_eq!(copy.len(), 1);
    assert!(copy.views().list().is_empty());

    let mem = f
        .cat
        .create(
            "mem",
            &CreateDataset {
                kind: DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    with_views(&mem);
    backup(&f, &mem, "mem");
    let copy = restore(&f, "mem", "mem-copy").unwrap();
    assert_eq!(definitions(&copy), definitions(&mem));
    assert_paused(&copy, "restored");
}

/// A clone carries the views paused, under its new dataset identity.
#[test]
fn clones_carry_paused_views() {
    let f = fixture();
    let ds = f.cat.create("wiki", &Default::default()).unwrap();
    with_views(&ds);
    let fork = f
        .cat
        .clone_dataset(
            "wiki",
            "fork",
            &CloneRequest::default(),
            &Default::default(),
        )
        .unwrap();
    assert_ne!(fork.dataset_id(), ds.dataset_id());
    assert_eq!(definitions(&fork), definitions(&ds));
    assert_paused(&fork, "cloned");
    fork.views().rebuild("sizes").unwrap();
    fork.views()
        .wait_built("sizes", Duration::from_secs(10))
        .unwrap();
    assert_eq!(rows(&fork, "sizes"), rows(&ds, "sizes"));
    // the clone's own backup carries the definitions it holds
    backup(&f, &fork, "fork");
    assert_eq!(
        manifest_json(&f, "fork")["requires"],
        serde_json::json!(["definitions"])
    );
}

fn manifest_path(f: &Fixture, name: &str) -> PathBuf {
    f.repo_dir.join("backups").join(format!("{name}.json"))
}

fn manifest_json(f: &Fixture, name: &str) -> J {
    serde_json::from_slice(&std::fs::read(manifest_path(f, name)).unwrap()).unwrap()
}

fn blob_path(repo: &Path, id: &str) -> PathBuf {
    repo.join("blobs").join(&id[..2]).join(id)
}

/// The index of the first definition entry of a manifest.
fn first_definition(m: &J) -> usize {
    m["files"]
        .as_array()
        .unwrap()
        .iter()
        .position(|f| f.get("definition").is_some())
        .unwrap()
}

/// A restore refuses a hostile or damaged definition entry before it publishes
/// anything: the target dataset does not exist afterwards, and no restore directory
/// is left behind.
#[test]
fn damaged_definition_entries_are_refused_before_publishing() {
    let f = fixture();
    let ds = f.cat.create("wiki", &Default::default()).unwrap();
    with_views(&ds);
    backup(&f, &ds, "b1");
    let good = manifest_json(&f, "b1");
    let i = first_definition(&good);
    let blob = good["files"][i]["blobs"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    type Edit = Box<dyn Fn(&mut J)>;
    let cases: Vec<(&str, &str, Edit)> = vec![
        (
            "traversal",
            "invalid-backup",
            Box::new(move |m| m["files"][i]["path"] = "views/../../escape/view.json".into()),
        ),
        (
            "duplicate",
            "invalid-backup",
            Box::new(move |m| {
                let dup = m["files"][i].clone();
                m["files"].as_array_mut().unwrap().push(dup);
            }),
        ),
        (
            "newer-format",
            "incompatible-format",
            Box::new(move |m| m["files"][i]["definition"]["format"] = 2.into()),
        ),
        (
            "unknown-kind",
            "incompatible-format",
            Box::new(move |m| {
                m["files"][i]["definition"]["kind"] = "analysis".into();
                m["files"][i]["path"] = "analyses/a/analysis.json".into();
            }),
        ),
        (
            "unknown-feature",
            "incompatible-format",
            Box::new(|m| m["requires"] = serde_json::json!(["definitions", "retained-history"])),
        ),
        (
            "missing-blob",
            "internal",
            Box::new(move |m| {
                m["files"][i]["blobs"][0]["id"] = "0".repeat(64).into();
            }),
        ),
        (
            "wrong-digest",
            "invalid-backup",
            Box::new(move |m| m["files"][i]["sha256"] = "1".repeat(64).into()),
        ),
    ];
    for (name, code, edit) in cases {
        let mut m = good.clone();
        m["name"] = name.into();
        m["id"] = uuid::Uuid::new_v4().to_string().into();
        edit(&mut m);
        std::fs::write(manifest_path(&f, name), serde_json::to_vec(&m).unwrap()).unwrap();
        let target = format!("t-{name}");
        match restore(&f, name, &target) {
            Err(Error::Component(c)) => assert_eq!(c.code, code, "{name}: {c}"),
            Err(e) => panic!("{name}: {e}"),
            Ok(_) => panic!("{name}: restored"),
        }
        assert!(f.cat.get(&target).is_none(), "{name}");
    }
    // a corrupt definition blob, also after the retries
    let path = blob_path(&f.repo_dir, &blob);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    match restore(&f, "b1", "t-corrupt") {
        Err(Error::Component(c)) => {
            assert!(c.to_string().contains("checksum mismatch"), "{c}")
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a corrupt definition restored"),
    }
    assert!(f.cat.get("t-corrupt").is_none());
    // nothing staged is left next to the databases
    let leftovers: Vec<String> = std::fs::read_dir(f.data.join("databases"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.') || n.starts_with("t-"))
                .collect()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
