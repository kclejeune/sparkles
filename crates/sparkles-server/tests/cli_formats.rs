//! The on-disk format fixtures of `testsuite/formats/` (X12 §4.6), run through the real
//! binary. Each fixture is a server data directory and backup repositories written by
//! an earlier release or commit (`scripts/format-fixture.sh`). This build must open
//! them as they are: `sparkles migrate --check` puts them inside the window, `sparkles
//! check` finds no errors, the recorded queries answer as recorded (past states and the
//! branch included), a write and a compaction succeed, the catalog opens, and the
//! backups restore to the same data. A fixture changed to carry a newer index format is
//! refused with a message that says so.

use serde_json::Value as J;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/formats");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("fixture.tar.gz").exists())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures in {}", dir.display());
    out
}

fn sparkles(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .unwrap()
}

#[track_caller]
fn ok(home: &Path, args: &[&str]) -> String {
    let o = sparkles(home, args);
    assert!(
        o.status.success(),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn unpack(fixture: &Path, into: &Path) {
    let s = Command::new("tar")
        .arg("-xzf")
        .arg(fixture.join("fixture.tar.gz"))
        .arg("-C")
        .arg(into)
        .status()
        .unwrap();
    assert!(s.success());
}

/// The part of a SPARQL JSON result that a later release must reproduce.
fn answer(json: &str) -> J {
    let v: J = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    match v.get("boolean") {
        Some(b) => b.clone(),
        None => v["results"]["bindings"].clone(),
    }
}

fn query(home: &Path, db: &str, fixture: &Path, q: &str, flags: &[&str]) -> J {
    let text = std::fs::read_to_string(fixture.join("queries").join(format!("{q}.rq"))).unwrap();
    let mut args = vec!["query", "--loc", db, "--results", "json"];
    args.extend_from_slice(flags);
    args.push(&text);
    answer(&ok(home, &args))
}

fn cases(fixture: &Path) -> Vec<(String, String, Vec<String>)> {
    std::fs::read_to_string(fixture.join("cases.txt"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut w = l.split_whitespace().map(str::to_string);
            let name = w.next().unwrap();
            let q = w.next().unwrap();
            (name, q, w.collect())
        })
        .collect()
}

fn check_answers(home: &Path, db: &str, fixture: &Path) {
    for (name, q, flags) in cases(fixture) {
        let expected =
            std::fs::read_to_string(fixture.join("expected").join(format!("{name}.json"))).unwrap();
        let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
        assert_eq!(
            query(home, db, fixture, &q, &flags),
            answer(&expected),
            "{}: case {name}",
            fixture.display()
        );
    }
}

fn count(home: &Path, db: &str, fixture: &Path) -> u64 {
    query(home, db, fixture, "count", &[])[0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn every_fixture_opens_answers_writes_and_restores() {
    for fixture in fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        unpack(&fixture, h);
        let data = h.join("data");
        let db_path = data.join("databases/books");
        let db = db_path.to_str().unwrap();
        let data_s = data.to_str().unwrap();

        // inside the window, with nothing that must be upgraded first
        let plan: J = serde_json::from_str(&ok(
            h,
            &["migrate", "--data", data_s, "--check", "--format", "json"],
        ))
        .unwrap();
        for d in plan["datasets"].as_array().unwrap() {
            assert_eq!(d["inWindow"], true, "{d}");
        }
        let o = sparkles(h, &["check", "--loc", db]);
        assert_ne!(
            o.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&o.stdout)
        );

        check_answers(h, db, &fixture);
        let stored = ok(
            h,
            &[
                "queries",
                "run",
                "--loc",
                db,
                "adults",
                "--set",
                "minAge=40",
            ],
        );
        assert_eq!(
            stored,
            std::fs::read_to_string(fixture.join("expected/stored-adults.txt")).unwrap()
        );

        // the catalog opens and lists the dataset
        let list = ok(
            h,
            &["dataset", "list", "--data-dir", data_s, "--format", "json"],
        );
        assert!(list.contains("books"), "{list}");

        // the backups restore to the same data
        let before = count(h, db, &fixture);
        let restored = h.join("restored");
        let repo = format!("file://{}", h.join("repo").display());
        ok(
            h,
            &[
                "backup",
                "restore",
                "--repo",
                &repo,
                "b1",
                "--to",
                restored.to_str().unwrap(),
            ],
        );
        assert_eq!(count(h, restored.to_str().unwrap(), &fixture), before);
        #[cfg(all(
            feature = "backup-encryption",
            any(target_os = "linux", target_os = "android")
        ))]
        if h.join("repo-encrypted").exists() {
            use std::os::unix::fs::PermissionsExt;
            let key = h.join("keys/recovery");
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
            let to = h.join("restored-encrypted");
            let repo = format!("file://{}", h.join("repo-encrypted").display());
            ok(
                h,
                &[
                    "backup",
                    "restore",
                    "--repo",
                    &repo,
                    "b1",
                    "--to",
                    to.to_str().unwrap(),
                    "--repository-key-file",
                    key.to_str().unwrap(),
                ],
            );
            assert_eq!(count(h, to.to_str().unwrap(), &fixture), before);
        }

        // a write and a compaction, then the database opens again
        ok(
            h,
            &[
                "update",
                "--loc",
                db,
                "INSERT DATA { <http://ex.org/new> <http://ex.org/p> 1 }",
            ],
        );
        ok(h, &["compact", "--loc", db]);
        assert_eq!(count(h, db, &fixture), before + 1);
        let o = sparkles(h, &["check", "--loc", db]);
        assert_ne!(
            o.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&o.stdout)
        );
        // the new generation's log has its header, and the dataset records this release
        let plan: J = serde_json::from_str(&ok(
            h,
            &["migrate", "--loc", db, "--check", "--format", "json"],
        ))
        .unwrap();
        let d = &plan["datasets"][0];
        assert_eq!(d["dataset"]["writtenBy"], env!("CARGO_PKG_VERSION"), "{d}");
        assert!(
            d["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["kind"] == "wal" && f["version"] == 1),
            "{d}"
        );
    }
}

#[test]
fn a_newer_index_format_is_refused_by_name() {
    let fixture = &fixtures()[0];
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    unpack(fixture, h);
    let data = h.join("data");
    let db = data.join("databases/books");
    let current = std::fs::read_to_string(db.join("CURRENT")).unwrap();
    let meta = db.join(current.trim()).join("meta.json");
    let mut m: J = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
    m["format_version"] = 99.into();
    std::fs::write(&meta, serde_json::to_vec(&m).unwrap()).unwrap();

    let o = sparkles(h, &["migrate", "--data", data.to_str().unwrap(), "--check"]);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("index version 99") && out.contains("newer release"),
        "{out}"
    );

    let o = sparkles(h, &["query", "--loc", db.to_str().unwrap(), "ASK {}"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("index version 99"), "{err}");

    // the catalog keeps serving without it, and keeps it registered
    let list = ok(
        h,
        &[
            "dataset",
            "list",
            "--data-dir",
            data.to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    assert!(!list.contains("\"books\""), "{list}");
    let config = std::fs::read_to_string(data.join("config.json")).unwrap();
    assert!(config.contains("books"), "{config}");
}
