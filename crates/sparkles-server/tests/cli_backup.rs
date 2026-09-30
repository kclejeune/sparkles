//! `sparkles backup`: the legacy N-Quads dump (`--loc DB --out DIR`, no subcommand)
//! next to the backup-repository subcommands, and `sparkles repo`.

#![cfg(feature = "backup")]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn database(dir: &Path) -> String {
    let nt = dir.join("d.nt");
    std::fs::write(&nt, "<urn:a> <urn:p> \"1\" .\n<urn:b> <urn:p> \"2\" .\n").unwrap();
    let db = dir.join("db");
    let o = run(&["load", "--loc", db.to_str().unwrap(), nt.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    db.to_str().unwrap().to_string()
}

#[test]
fn the_legacy_dump_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let out = dir.path().join("out");
    let o = run(&["backup", "--loc", &db, "--out", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("backup written to"), "{}", stderr(&o));
    let files: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    assert!(
        files[0].file_name().to_string_lossy().ends_with(".nq.gz"),
        "{:?}",
        files[0].file_name()
    );
    // with a codec
    let o = run(&[
        "backup",
        "--loc",
        &db,
        "--out",
        out.to_str().unwrap(),
        "--compress",
        "zstd",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));

    // without --loc and without a subcommand: a usage error
    let o = run(&["backup"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("--loc"), "{}", stderr(&o));
    // the legacy flags do not mix with subcommands
    let o = run(&["backup", "--loc", &db, "list", "--repo", "x"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!stderr(&o).contains("not implemented"), "{}", stderr(&o));
}

#[test]
fn subcommands_parse() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["backup", "list", "--repo", "file:///tmp/r"],
        vec![
            "backup", "create", "--loc", "db", "--repo", "local", "--name", "b1",
        ],
        vec![
            "backup", "show", "--repo", "local", "b1", "--format", "json",
        ],
        vec!["backup", "delete", "--repo", "local", "b1"],
        vec![
            "backup", "restore", "--repo", "local", "b1", "--to", "/tmp/x",
        ],
        vec![
            "backup",
            "restore",
            "--repo",
            "local",
            "b1",
            "--data",
            "/tmp/d",
            "--as",
            "ds",
            "--identity",
            "new",
            "--check",
            "full",
        ],
        vec![
            "backup", "verify", "--repo", "local", "b1", "--level", "restore",
        ],
        vec![
            "backup",
            "policy",
            "preview",
            "30 2 * * *",
            "--tz",
            "Europe/Berlin",
        ],
        vec!["repo", "add", "local", "--path", "/srv/r"],
        vec![
            "repo",
            "add",
            "s3",
            "--s3",
            "bucket",
            "--prefix",
            "p",
            "--path-style",
        ],
        vec!["repo", "list", "--format", "json"],
        vec!["repo", "verify", "local", "--level", "data"],
        vec!["repo", "gc", "local", "--dry-run", "--grace", "1h"],
        vec!["repo", "locks", "local", "--break", "abc"],
    ] {
        let mut args = args;
        let cfg = dir.path().join("backup.toml");
        args.extend(["--backup-config", cfg.to_str().unwrap()]);
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("not implemented yet"),
            "{args:?}: {}",
            stderr(&o)
        );
    }
    // clap refuses bad combinations before anything runs
    for args in [
        vec!["backup", "restore", "--repo", "r", "b1"],
        vec![
            "backup", "restore", "--repo", "r", "b1", "--to", "x", "--data", "y",
        ],
        vec![
            "backup",
            "restore",
            "--repo",
            "r",
            "b1",
            "--to",
            "x",
            "--identity",
            "maybe",
        ],
        vec!["repo", "add", "x"],
        vec!["repo", "add", "x", "--path", "/p", "--s3", "b"],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(
            !stderr(&o).contains("not implemented"),
            "{args:?}: {}",
            stderr(&o)
        );
    }
}
