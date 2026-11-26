//! End-to-end CLI tests for `sparkles git export`, `sparkles git write-tree`, and
//! `sparkles git verify`.

use sparkles::Dataset;
use sparkles::io::RdfFormat;
use std::fs;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

#[test]
fn cli_git_export_write_tree_and_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    let tree_dir = tmp.path().join("tree");
    let repo_dir = tmp.path().join("repo");

    {
        let ds = Dataset::open(&db).unwrap();
        ds.set_prefix("ex", "http://example.org/").unwrap();
        ds.load_str(
            "PREFIX ex: <http://example.org/> ex:a ex:p 1 .",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.update("INSERT DATA { GRAPH <http://example.org/g1> { <http://example.org/b> <http://example.org/q> \"hi\" } }")
            .unwrap();
    }

    // 1. Write canonical tree and verify it.
    let write_out = Command::new(BIN)
        .args([
            "git",
            "write-tree",
            "--loc",
            db.to_str().unwrap(),
            "--out",
            tree_dir.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        write_out.status.success(),
        "{}",
        String::from_utf8_lossy(&write_out.stderr)
    );
    let write_json: serde_json::Value = serde_json::from_slice(&write_out.stdout).unwrap();
    assert_eq!(write_json["manifest"]["format_version"], 1);
    assert_eq!(
        write_json["manifest"]["graphs"].as_array().unwrap().len(),
        2
    );

    let verify_out = Command::new(BIN)
        .args([
            "git",
            "verify",
            "--dir",
            tree_dir.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        verify_out.status.success(),
        "{}",
        String::from_utf8_lossy(&verify_out.stderr)
    );
    let verify_json: serde_json::Value = serde_json::from_slice(&verify_out.stdout).unwrap();
    assert_eq!(verify_json["valid"], true);
    assert_eq!(verify_json["format_version"], 1);
    assert!(verify_json.get("formatVersion").is_none());
    assert_eq!(verify_json["quads"], 2);

    // Unknown format_version must fail with non-zero exit.
    fs::write(
        tree_dir.join(".sparkles-git/manifest.json"),
        r#"{"format_version": 99}"#,
    )
    .unwrap();
    let bad_verify = Command::new(BIN)
        .args(["git", "verify", "--dir", tree_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad_verify.status.success());
    assert!(
        String::from_utf8_lossy(&bad_verify.stderr).contains("unsupported format_version 99"),
        "{}",
        String::from_utf8_lossy(&bad_verify.stderr)
    );

    // 2. Export branch to Git repository and verify JSON report + working tree.
    let export_out = Command::new(BIN)
        .args([
            "git",
            "export",
            "--loc",
            db.to_str().unwrap(),
            "--out",
            repo_dir.to_str().unwrap(),
            "--mode",
            "commits",
            "--syntax",
            "turtle",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        export_out.status.success(),
        "{}",
        String::from_utf8_lossy(&export_out.stderr)
    );
    let export_json: serde_json::Value = serde_json::from_slice(&export_out.stdout).unwrap();
    assert_eq!(export_json["commits_exported"], 2);
    assert_eq!(export_json["mapping"]["format_version"], 1);
    assert_eq!(
        export_json["mapping"]["entries"].as_array().unwrap().len(),
        2
    );

    let verify_repo = Command::new(BIN)
        .args(["git", "verify", "--dir", repo_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        verify_repo.status.success(),
        "{}",
        String::from_utf8_lossy(&verify_repo.stderr)
    );
}

#[test]
fn cli_git_supports_branch_option_and_scrubs_git_env() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("db");
    let tree_dir = tmp.path().join("branch-tree");
    let repo_dir = tmp.path().join("branch-repo");
    let decoy_repo = tmp.path().join("decoy-repo");

    // Create a decoy repo to verify GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE are scrubbed.
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main", decoy_repo.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );

    {
        let ds = Dataset::open(&db).unwrap();
        ds.set_prefix("ex", "http://example.org/").unwrap();
        ds.load_str(
            "PREFIX ex: <http://example.org/> ex:mainOnly ex:p 1 .",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.create_branch("vocab-dev", &sparkles::branch::BranchOptions::default())
            .unwrap();
        let dev = ds.branch("vocab-dev").unwrap();
        dev.load_str(
            "PREFIX ex: <http://example.org/> ex:devOnly ex:p 2 .",
            RdfFormat::Turtle,
        )
        .unwrap();
    }

    // 1. `--branch vocab-dev git write-tree --syntax nquads`
    let wt = Command::new(BIN)
        .args([
            "--branch",
            "vocab-dev",
            "git",
            "write-tree",
            "--loc",
            db.to_str().unwrap(),
            "--out",
            tree_dir.to_str().unwrap(),
            "--syntax",
            "nquads",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        wt.status.success(),
        "{}",
        String::from_utf8_lossy(&wt.stderr)
    );
    let wt_json: serde_json::Value = serde_json::from_slice(&wt.stdout).unwrap();
    assert_eq!(wt_json["branch"], "vocab-dev");
    assert_eq!(wt_json["manifest"]["syntax"], "nquads");
    assert_eq!(wt_json["manifest"]["graphs"][0]["quads"], 2);

    // 2. `--branch` is rejected on `git verify` (operates on a directory, not a DB).
    let bad_verify_branch = Command::new(BIN)
        .args([
            "--branch",
            "vocab-dev",
            "git",
            "verify",
            "--dir",
            tree_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!bad_verify_branch.status.success());

    // 3. `--branch vocab-dev git export --git-branch release/vocab` with hostile Git env.
    let exp = Command::new(BIN)
        .env("GIT_DIR", decoy_repo.join(".git"))
        .env("GIT_WORK_TREE", &decoy_repo)
        .env("GIT_INDEX_FILE", decoy_repo.join(".git/index"))
        .args([
            "--branch",
            "vocab-dev",
            "git",
            "export",
            "--loc",
            db.to_str().unwrap(),
            "--out",
            repo_dir.to_str().unwrap(),
            "--git-branch",
            "release/vocab",
            "--mode",
            "commits",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        exp.status.success(),
        "{}",
        String::from_utf8_lossy(&exp.stderr)
    );
    let exp_json: serde_json::Value = serde_json::from_slice(&exp.stdout).unwrap();
    assert_eq!(exp_json["branch"], "vocab-dev");
    assert_eq!(exp_json["git_branch"], "release/vocab");
    assert_eq!(exp_json["commits_exported"], 2);

    // Decoy repo must still have no commits!
    let decoy_head = Command::new("git")
        .args([
            "-C",
            decoy_repo.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(
        !decoy_head.status.success(),
        "hostile GIT_DIR leaked into decoy repo!"
    );

    // 4. Verify `--no-checksums` works via CLI too.
    let ver = Command::new(BIN)
        .args([
            "git",
            "verify",
            "--dir",
            repo_dir.to_str().unwrap(),
            "--no-checksums",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        ver.status.success(),
        "{}",
        String::from_utf8_lossy(&ver.stderr)
    );
    let ver_json: serde_json::Value = serde_json::from_slice(&ver.stdout).unwrap();
    assert_eq!(ver_json["format_version"], 1);
    assert_eq!(ver_json["quads"], 2);
}
