//! `sparkles backup`: the legacy N-Quads dump (`--loc DB --out DIR`, no subcommand)
//! next to the backup-repository subcommands, and `sparkles repo`, run as the real
//! binary against `fs` and `memory://` repositories in temporary directories.

#![cfg(feature = "backup")]

use serde_json::Value as J;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// The binary with its home, config and cache directories inside `home`.
fn sparkles(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("SPARKLES_BACKUP_CONFIG");
    c
}

fn run_in(home: &Path, args: &[&str]) -> Output {
    sparkles(home).args(args).output().unwrap()
}

fn run(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    run_in(home.path(), args)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Run and expect exit status `code`.
#[track_caller]
fn expect(home: &Path, args: &[&str], code: i32) -> Output {
    let o = run_in(home, args);
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        stdout(&o),
        stderr(&o)
    );
    o
}

#[track_caller]
fn json_of(o: &Output) -> J {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}\n{}", stdout(o), stderr(o)))
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn file_url(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn database(dir: &Path) -> String {
    let nt = dir.join("d.nt");
    std::fs::write(&nt, "<urn:a> <urn:p> \"1\" .\n<urn:b> <urn:p> \"2\" .\n").unwrap();
    let db = dir.join("db");
    let o = run(&["load", "--loc", s(&db), s(&nt)]);
    assert!(o.status.success(), "{}", stderr(&o));
    s(&db).to_string()
}

/// The quad count of the database at `loc`.
fn count(home: &Path, loc: &Path) -> u64 {
    let o = expect(
        home,
        &[
            "query",
            "--loc",
            s(loc),
            "--results",
            "json",
            "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }",
        ],
        0,
    );
    json_of(&o)["results"]["bindings"][0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A database `db` with backups `b1` (2 quads) and `b2` (2 quads, one changed) in the
/// fs repository `r`; returns (db, repository URL).
fn two_backups(home: &Path) -> (PathBuf, String) {
    let db = PathBuf::from(database(home));
    let url = file_url(&home.join("r"));
    let o = expect(
        home,
        &[
            "backup",
            "create",
            "--loc",
            s(&db),
            "--repo",
            &url,
            "--name",
            "b1",
        ],
        0,
    );
    assert!(
        stdout(&o).starts_with("backup b1 of db ("),
        "{}",
        stdout(&o)
    );
    expect(
        home,
        &[
            "update",
            "--loc",
            s(&db),
            "DELETE DATA { <urn:a> <urn:p> \"1\" } ; INSERT DATA { <urn:c> <urn:p> \"3\" }",
        ],
        0,
    );
    expect(
        home,
        &[
            "backup",
            "create",
            "--loc",
            s(&db),
            "--repo",
            &url,
            "--name",
            "b2",
            "--note",
            "second",
        ],
        0,
    );
    (db, url)
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
    // zstd by default
    assert!(
        files[0].file_name().to_string_lossy().ends_with(".nq.zst"),
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
        "gzip",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let gz = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().ends_with(".nq.gz"))
        .count();
    assert_eq!(gz, 1);

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
fn bad_combinations_are_usage_errors() {
    // clap refuses them before anything runs
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
        vec![
            "backup", "restore", "--repo", "r", "b1", "--to", "x", "--as", "y",
        ],
        vec!["backup", "list", "--repo", "r", "--format", "yaml"],
        vec!["repo", "add", "x"],
        vec!["repo", "add", "x", "--path", "/p", "--s3", "b"],
        vec!["repo", "add", "x", "--path", "/p", "--prefix", "p"],
        vec!["repo", "add", "x", "--path", "/p", "--credentials", "env"],
        vec![
            "repo",
            "add",
            "x",
            "--path",
            "/p",
            "--credentials-name",
            "c",
        ],
        vec![
            "backup",
            "restore",
            "--repo",
            "r",
            "b1",
            "--data",
            "x",
            "--replace",
        ],
        vec!["repo", "verify", "x", "--level", "restore"],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).starts_with("error:"), "{args:?}: {}", stderr(&o));
    }
}

#[test]
fn repositories_are_named_or_given_by_url() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // no config file
    let o = expect(h, &["backup", "list", "--repo", "local"], 1);
    assert!(stderr(&o).contains("does not exist"), "{}", stderr(&o));
    // credentials never go in URLs
    let o = expect(
        h,
        &["backup", "list", "--repo", "s3://key:secret@bucket/p"],
        1,
    );
    assert!(stderr(&o).contains("credentials"), "{}", stderr(&o));
    // listing never initializes a location
    let empty = h.join("empty");
    let o = expect(h, &["backup", "list", "--repo", &file_url(&empty)], 1);
    assert!(!empty.join("sparkles-repo.json").exists());
    assert!(!stderr(&o).is_empty());
}

/// `repo add --credentials-name` names a credential source of the config file: one
/// that is not defined needs `--credentials`, one that is must match it. Nothing is
/// written (or connected to) when they do not.
#[test]
fn named_credential_sources_are_checked_before_connecting() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let cfg = h.join("config/sparkles/backup.toml");
    let add = |extra: &[&str]| -> Output {
        let mut args = vec!["repo", "add", "s3-main", "--s3", "kg"];
        args.extend_from_slice(extra);
        expect(h, &args, 1)
    };
    let o = add(&["--credentials-name", "minio"]);
    assert!(
        stderr(&o).contains("has no [credentials.minio]"),
        "{}",
        stderr(&o)
    );
    assert!(!cfg.exists());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        "version = 1\n\n[credentials.minio]\nsource = \"env\"\naccess_key_id_var = \"K\"\nsecret_access_key_var = \"S\"\n",
    )
    .unwrap();
    let before = std::fs::read(&cfg).unwrap();
    let o = add(&["--credentials-name", "minio", "--credentials", "default"]);
    assert!(stderr(&o).contains("otherwise"), "{}", stderr(&o));
    // a bad name, before any connection
    let o = add(&["--credentials-name", "Not Valid", "--credentials", "env"]);
    assert!(
        stderr(&o).contains("invalid credential source name"),
        "{}",
        stderr(&o)
    );
    assert_eq!(std::fs::read(&cfg).unwrap(), before);
}

/// Offline disaster recovery: list and restore from a repository with the server
/// stopped, restore into a stopped server's data directory, and the refusals while
/// the server runs.
#[test]
fn offline_disaster_recovery() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let (_db, url) = two_backups(h);

    let o = expect(h, &["backup", "list", "--repo", &url], 0);
    let text = stdout(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("name  dataset  commit"), "{text}");
    assert!(lines[1].starts_with("b2    db"), "{text}");
    assert!(lines[2].starts_with("b1    db"), "{text}");
    assert!(
        lines[3].starts_with("repository ") && lines[3].contains("2 backups · 1 dataset"),
        "{text}"
    );
    let list = json_of(&expect(h, &["backup", "list", "--repo", &url, "--json"], 0));
    let backups = list["backups"].as_array().unwrap();
    assert_eq!(backups.len(), 2);
    assert_eq!(backups[0]["name"], "b2");
    assert_eq!(backups[0]["note"], "second");
    let b2_seq = backups[0]["commit"]["seq"].as_u64().unwrap();
    assert!(b2_seq > backups[1]["commit"]["seq"].as_u64().unwrap());
    let filtered = json_of(&expect(
        h,
        &[
            "backup",
            "list",
            "--repo",
            &url,
            "--dataset",
            "other",
            "--format",
            "json",
        ],
        0,
    ));
    assert_eq!(filtered["backups"].as_array().unwrap().len(), 0);
    let show = json_of(&expect(
        h,
        &["backup", "show", "--repo", &url, "b2", "--json"],
        0,
    ));
    assert_eq!(show["name"], "b2");
    assert!(!show["files"].as_array().unwrap().is_empty());
    let o = expect(h, &["backup", "show", "--repo", &url, "b2"], 0);
    assert!(
        stdout(&o)
            .lines()
            .any(|l| l.starts_with("commits.bin ") && l.contains(" append ")),
        "{}",
        stdout(&o)
    );
    expect(h, &["backup", "show", "--repo", &url, "b9"], 1);

    // restore into a directory, then query and check it
    let dr = h.join("dr").join("ds");
    let o = expect(
        h,
        &["backup", "restore", "--repo", &url, "b2", "--to", s(&dr)],
        0,
    );
    assert!(
        stdout(&o).starts_with("restored b2 of db"),
        "{}",
        stdout(&o)
    );
    assert!(stdout(&o).contains("(kept)"), "{}", stdout(&o));
    assert_eq!(count(h, &dr), 2);
    expect(h, &["check", "--loc", s(&dr)], 0);
    // a database is only replaced on request
    let o = expect(
        h,
        &["backup", "restore", "--repo", &url, "b1", "--to", s(&dr)],
        1,
    );
    assert!(stderr(&o).contains("--replace"), "{}", stderr(&o));
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b1",
            "--to",
            s(&dr),
            "--replace",
            "--identity",
            "new",
            "--check",
            "full",
            "--json",
        ],
        0,
    );
    let r = json_of(&o);
    assert_eq!(r["identity"], "new");
    assert_eq!(r["forkedFrom"]["seq"], backups[1]["commit"]["seq"]);
    expect(h, &["check", "--loc", s(&dr)], 0);
    let leftovers: Vec<_> = std::fs::read_dir(h.join("dr"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("ds")]);

    // into a stopped server's data directory
    let data = h.join("data");
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b2",
            "--data",
            s(&data),
            "--as",
            "ds",
        ],
        0,
    );
    assert!(stdout(&o).contains("as /ds"), "{}", stdout(&o));
    let reg: J = serde_json::from_slice(&std::fs::read(data.join("config.json")).unwrap()).unwrap();
    assert_eq!(reg["datasets"][0]["name"], "ds");
    // the name is taken now
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b1",
            "--data",
            s(&data),
            "--as",
            "ds",
        ],
        1,
    );
    assert!(stderr(&o).contains("already registered"), "{}", stderr(&o));
    // the backup's dataset name by default; the source id is taken, so `auto` mints one
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b1",
            "--data",
            s(&data),
            "--json",
        ],
        0,
    );
    let r = json_of(&o);
    assert_eq!(r["dataset"], "db");
    assert_eq!(r["identity"], "new");

    let server = Server::start(&data);
    let info = server.get("/$/datasets/ds");
    assert_eq!(info["head"].as_u64(), Some(b2_seq), "{info}");
    assert_eq!(info["quads"].as_u64(), Some(2), "{info}");
    assert!(server.get("/$/datasets/db")["id"].is_string());

    // while it runs: its data directory and its databases are refused
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b2",
            "--data",
            s(&data),
            "--as",
            "ds2",
        ],
        1,
    );
    assert!(stderr(&o).contains("in use"), "{}", stderr(&o));
    assert!(!data.join("databases").join("ds2").exists());
    let live = data.join("databases").join("ds");
    let o = expect(
        h,
        &[
            "backup",
            "restore",
            "--repo",
            &url,
            "b1",
            "--to",
            s(&live),
            "--replace",
        ],
        1,
    );
    assert!(
        stderr(&o).contains("in use by another process"),
        "{}",
        stderr(&o)
    );
    let o = expect(
        h,
        &["backup", "create", "--loc", s(&live), "--repo", &url],
        1,
    );
    assert!(
        stderr(&o).contains("in use by another process"),
        "{}",
        stderr(&o)
    );
    drop(server);
    assert_eq!(count(h, &live), 2);
}

#[test]
fn verify_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let (_db, url) = two_backups(h);
    let o = expect(h, &["repo", "verify", &url], 0);
    assert!(stdout(&o).contains("b2  ok"), "{}", stdout(&o));
    expect(h, &["repo", "verify", &url, "--level", "data"], 0);
    let o = expect(
        h,
        &[
            "backup", "verify", "--repo", &url, "b2", "--level", "restore",
        ],
        0,
    );
    assert!(stdout(&o).starts_with("b2  ok"), "{}", stdout(&o));

    // deleting b2 orphans the blobs only it used: a warning
    expect(h, &["backup", "delete", "--repo", &url, "b2"], 0);
    expect(h, &["backup", "delete", "--repo", &url, "b2"], 1);
    let o = expect(h, &["repo", "verify", &url, "--json"], 2);
    let v = json_of(&o);
    assert_eq!(v["status"], "warning");
    assert!(v["orphans"]["blobs"].as_u64().unwrap() > 0, "{v}");
    let o = expect(h, &["repo", "verify", &url], 2);
    assert!(stdout(&o).contains("orphans:"), "{}", stdout(&o));
    // GC keeps them during the grace period, then collects them
    let g = json_of(&expect(h, &["repo", "gc", &url, "--dry-run", "--json"], 0));
    assert_eq!(g["candidates"], 0);
    assert!(g["keptYoung"].as_u64().unwrap() > 0, "{g}");
    std::thread::sleep(Duration::from_millis(2100));
    let o = expect(h, &["repo", "gc", &url, "--grace", "1s", "--dry-run"], 0);
    assert!(stdout(&o).contains("would delete"), "{}", stdout(&o));
    let g = json_of(&expect(
        h,
        &["repo", "gc", &url, "--grace", "1s", "--json"],
        0,
    ));
    assert!(g["deleted"].as_u64().unwrap() > 0, "{g}");
    expect(h, &["repo", "verify", &url], 0);
    expect(h, &["repo", "gc", &url, "--grace", "soon"], 1);

    // a missing blob is an error
    let blob = first_file(&h.join("r").join("blobs"));
    std::fs::remove_file(&blob).unwrap();
    let o = expect(h, &["repo", "verify", &url], 1);
    assert!(stdout(&o).contains("b1  error"), "{}", stdout(&o));
    expect(h, &["backup", "verify", "--repo", &url, "b1"], 1);
    expect(h, &["backup", "verify", "--repo", &url, "b9"], 1);

    // no locks are left behind
    let o = expect(h, &["repo", "locks", &url], 0);
    assert_eq!(stdout(&o).trim(), "no locks");
    expect(h, &["repo", "locks", &url, "--break", "nope"], 1);
}

/// Some file under `dir`, searching every subdirectory (GC can leave empty ones behind).
fn first_file(dir: &Path) -> PathBuf {
    fn find(dir: &Path) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_file() {
                return Some(p);
            }
            if p.is_dir()
                && let Some(f) = find(&p)
            {
                return Some(f);
            }
        }
        None
    }
    find(dir).unwrap_or_else(|| panic!("no file under {}", dir.display()))
}

#[test]
fn the_config_file_is_rewritten_in_place() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let cfg = h.join("backup.toml");
    let main = h.join("main-repo");
    std::fs::write(
        &cfg,
        format!(
            "# my comment\nversion = 1\n\n[repositories.main]\ntype = \"fs\"\npath = \"{}\"\n\n[policies.nightly]\nrepository = \"main\"\nschedule = \"30 2 * * *\"\ntimezone = \"Europe/Berlin\"\nretention = {{ expire_after = \"30d\", min_count = 7 }}\n",
            main.display()
        ),
    )
    .unwrap();
    let c = s(&cfg);
    let local = h.join("local-repo");
    let o = expect(
        h,
        &[
            "repo",
            "add",
            "local",
            "--path",
            s(&local),
            "--backup-config",
            c,
        ],
        0,
    );
    assert!(
        stdout(&o).starts_with("added repository local (fs "),
        "{}",
        stdout(&o)
    );
    assert!(local.join("sparkles-repo.json").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(text.starts_with("# Sparkles backup"), "{text}");
    assert!(!text.contains("my comment"), "{text}");
    for want in [
        "[repositories.local]",
        "[repositories.main]",
        "[policies.nightly]",
        "Europe/Berlin",
        "min_count = 7",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    // names are unique
    expect(
        h,
        &[
            "repo",
            "add",
            "local",
            "--path",
            "/elsewhere",
            "--backup-config",
            c,
        ],
        1,
    );
    expect(
        h,
        &[
            "repo",
            "add",
            "nightly",
            "--path",
            "/elsewhere",
            "--backup-config",
            c,
        ],
        1,
    );
    // and so are locations
    expect(
        h,
        &[
            "repo",
            "add",
            "again",
            "--path",
            s(&local),
            "--backup-config",
            c,
        ],
        1,
    );
    // an attach-only add of an empty location fails and changes nothing
    let o = expect(
        h,
        &[
            "repo",
            "add",
            "fresh",
            "--path",
            s(&h.join("fresh")),
            "--no-init",
            "--backup-config",
            c,
        ],
        1,
    );
    assert!(!stderr(&o).is_empty());
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), text);

    let list = json_of(&expect(
        h,
        &["repo", "list", "--backup-config", c, "--json"],
        0,
    ));
    let names: Vec<&str> = list["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["local", "main"]);
    let o = expect(h, &["repo", "list", "--backup-config", c], 0);
    assert!(stdout(&o).contains("local  fs"), "{}", stdout(&o));

    let o = expect(h, &["repo", "show", "local", "--backup-config", c], 0);
    assert!(stdout(&o).contains("reachable  yes"), "{}", stdout(&o));
    let v = json_of(&expect(
        h,
        &["repo", "show", "main", "--backup-config", c, "--json"],
        1,
    ));
    // `main` was never initialized
    assert_eq!(v["status"]["reachable"], false);
    assert_eq!(v["policies"][0], "nightly");
    let t = json_of(&expect(
        h,
        &["repo", "test", "local", "--backup-config", c, "--json"],
        0,
    ));
    assert_eq!(t["ok"], true);
    assert_eq!(t["conditionalWrites"], true);

    // a backup through the name, cached under XDG_CACHE_HOME
    let db = database(h);
    expect(
        h,
        &[
            "backup",
            "create",
            "--loc",
            &db,
            "--repo",
            "local",
            "--backup-config",
            c,
        ],
        0,
    );
    let o = expect(
        h,
        &["backup", "list", "--repo", "local", "--backup-config", c],
        0,
    );
    assert!(
        stdout(&o).lines().nth(1).unwrap().starts_with("db-"),
        "{}",
        stdout(&o)
    );
    assert!(
        h.join("cache/sparkles/backup")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );

    // policies
    let o = expect(h, &["backup", "policy", "list", "--backup-config", c], 0);
    assert!(stdout(&o).contains("nightly  main"), "{}", stdout(&o));
    let p = json_of(&expect(
        h,
        &[
            "backup",
            "policy",
            "show",
            "nightly",
            "--backup-config",
            c,
            "--json",
        ],
        0,
    ));
    assert_eq!(p["next"].as_array().unwrap().len(), 5);
    assert_eq!(p["retention"]["minCount"], 7);
    expect(
        h,
        &["backup", "policy", "show", "weekly", "--backup-config", c],
        1,
    );
    // its repository was never initialized
    expect(
        h,
        &[
            "backup",
            "policy",
            "history",
            "nightly",
            "--backup-config",
            c,
        ],
        1,
    );
    expect(
        h,
        &["backup", "policy", "run", "nightly", "--backup-config", c],
        1,
    );

    // a repository a policy uses stays
    let o = expect(h, &["repo", "remove", "main", "--backup-config", c], 1);
    assert!(stderr(&o).contains("nightly"), "{}", stderr(&o));
    expect(h, &["repo", "remove", "local", "--backup-config", c], 0);
    expect(h, &["repo", "remove", "local", "--backup-config", c], 1);
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(!text.contains("[repositories.local]"), "{text}");
    assert!(text.contains("[repositories.main]") && text.contains("[policies.nightly]"));
    // the repository's contents stay
    assert!(local.join("backups").is_dir());

    // the default file is $XDG_CONFIG_HOME/sparkles/backup.toml
    expect(h, &["repo", "add", "x", "--path", s(&h.join("x"))], 0);
    assert!(h.join("config/sparkles/backup.toml").is_file());
    let o = expect(h, &["repo", "list"], 0);
    assert!(stdout(&o).contains("x     fs"), "{}", stdout(&o));
    // the environment variable names a file too
    let o = sparkles(h)
        .env("SPARKLES_BACKUP_CONFIG", &cfg)
        .args(["repo", "list"])
        .output()
        .unwrap();
    assert!(stdout(&o).contains("main"), "{}", stdout(&o));
}

#[test]
fn schedule_preview() {
    let o = run(&[
        "backup",
        "policy",
        "preview",
        "30 2 * * *",
        "--tz",
        "Europe/Berlin",
        "--json",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let p = json_of(&o);
    assert_eq!(p["next"].as_array().unwrap().len(), 5);
    assert!(p["description"].as_str().unwrap().contains("02:30"), "{p}");
    let o = run(&["backup", "policy", "preview", "every 6h", "--count", "2"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o).lines().count(), 3, "{}", stdout(&o));
    let o = run(&["backup", "policy", "preview", "61 * * * *"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("invalid schedule"), "{}", stderr(&o));
}

#[test]
fn memory_repositories() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let t = json_of(&expect(h, &["repo", "test", "memory://", "--json"], 0));
    assert_eq!(t["ok"], true);
    let o = expect(h, &["repo", "test", "memory://"], 0);
    assert!(stdout(&o).contains("create-again"), "{}", stdout(&o));
    let db = database(h);
    let o = expect(
        h,
        &[
            "backup",
            "create",
            "--loc",
            &db,
            "--repo",
            "memory://",
            "--name",
            "m1",
            "--json",
        ],
        0,
    );
    let b = json_of(&o);
    assert_eq!(b["name"], "m1");
    assert!(b["stats"]["files"].as_u64().unwrap() > 0, "{b}");
    // backup names take upper case too, and the message says so
    for (name, code) in [("Nightly.2026-09-30", 0), (".bad", 1), ("a b", 1)] {
        let o = expect(
            h,
            &[
                "backup",
                "create",
                "--loc",
                &db,
                "--repo",
                "memory://",
                "--name",
                name,
            ],
            code,
        );
        if code == 1 {
            assert!(
                stderr(&o).contains("A-Z, a-z, 0-9, '.', '_' and '-'"),
                "{}",
                stderr(&o)
            );
        }
    }
    // progress went to stderr
    assert!(stderr(&o).contains('%'), "{}", stderr(&o));
    // each process has its own memory repository
    let o = expect(h, &["backup", "list", "--repo", "memory://"], 0);
    assert!(stdout(&o).starts_with("no backups"), "{}", stdout(&o));
    expect(h, &["repo", "verify", "memory://"], 0);
    expect(h, &["backup", "show", "--repo", "memory://", "m1"], 1);
    expect(h, &["repo", "gc", "memory://", "--dry-run"], 0);
}

/// A server on a data directory, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(data: &Path) -> Server {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(data)
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let s = Server { child, port };
        let t0 = Instant::now();
        while s.try_get("/$/ping").is_none() {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    fn try_get(&self, path: &str) -> Option<String> {
        let mut c = std::net::TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        write!(c, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
        let mut buf = String::new();
        c.read_to_string(&mut buf).ok()?;
        let (head, body) = buf.split_once("\r\n\r\n")?;
        head.starts_with("HTTP/1.0 200")
            .then(|| body.to_string())
            .or_else(|| head.starts_with("HTTP/1.1 200").then(|| body.to_string()))
    }

    fn get(&self, path: &str) -> J {
        let body = self.try_get(path).unwrap_or_else(|| panic!("GET {path}"));
        serde_json::from_str(&body).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
