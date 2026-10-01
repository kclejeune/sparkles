//! `sparkles fmt` run as the real binary in temporary directories: modes and exit codes,
//! walks and ignore files, config discovery, `--stdin-filepath`, and `--write`'s atomic
//! replacement.
//!
//! "Unformatted" inputs here start with a byte order mark, which formatting always drops;
//! "formatted" inputs are the formatter's own output, so these tests hold whatever the
//! formatting rules print.

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const QUERY: &str = "PREFIX ex: <http://example.org/>\nSELECT ?s WHERE { ?s ex:p ?o }\n";
const BOM: &str = "\u{feff}";
/// A syntax error at 3:14.
const BAD: &str = "SELECT ?s\nWHERE {\n  BIND(10 AS 2)\n}\n";

/// `sparkles fmt ARGS` in `dir`, with stdin, and the home and config directories (the
/// global gitignore) inside it.
fn fmt_in(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(BIN);
    c.arg("fmt")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    drop(input);
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Run and expect exit status `code`.
#[track_caller]
fn expect(dir: &Path, args: &[&str], stdin: Option<&str>, code: i32) -> Output {
    let o = fmt_in(dir, args, stdin);
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        stdout(&o),
        stderr(&o)
    );
    o
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The formatter's output for `text` (a SPARQL document).
fn formatted(text: &str) -> String {
    let d = tempfile::tempdir().unwrap();
    stdout(&expect(d.path(), &["--language", "sparql"], Some(text), 0))
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn prints_writes_and_leaves_formatted_files_alone() {
    let d = tempdir();
    let dir = d.path();
    let unformatted = format!("{BOM}{QUERY}");
    let pretty = formatted(&unformatted);
    assert!(!pretty.starts_with(BOM));
    write(dir, "q.rq", &unformatted);
    // printing never touches the file
    let o = expect(dir, &["q.rq"], None, 0);
    assert_eq!(stdout(&o), pretty);
    assert_eq!(read(dir, "q.rq"), unformatted);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("q.rq"), std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let before = std::fs::metadata(dir.join("q.rq")).unwrap();
    let o = expect(dir, &["--write", "q.rq"], None, 0);
    assert_eq!(stdout(&o), "");
    assert_eq!(read(dir, "q.rq"), pretty);
    let after = std::fs::metadata(dir.join("q.rq")).unwrap();
    assert_eq!(before.permissions(), after.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // replaced by a rename, not rewritten in place
        assert_ne!(before.ino(), after.ino());
    }
    // no temporary file is left behind
    let names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["q.rq"]);

    // a second --write leaves the file alone: same mtime, same inode
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(dir.join("q.rq"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let before = std::fs::metadata(dir.join("q.rq")).unwrap();
    expect(dir, &["--write", "q.rq"], None, 0);
    let after = std::fs::metadata(dir.join("q.rq")).unwrap();
    assert_eq!(after.modified().unwrap(), old);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), after.ino());
    }
    expect(dir, &["--check", "q.rq"], None, 0);
}

#[test]
fn write_refuses_errors_and_writes_through_links() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "bad.rq", &format!("{BOM}{BAD}"));
    write(dir, "real/q.rq", &format!("{BOM}{QUERY}"));
    let o = expect(dir, &["--write", "bad.rq", "real/q.rq"], None, 2);
    assert!(
        stderr(&o).contains("bad.rq:3:14: error: "),
        "{}",
        stderr(&o)
    );
    // the error leaves its file as it was; the other file is still written
    assert_eq!(read(dir, "bad.rq"), format!("{BOM}{BAD}"));
    assert_eq!(read(dir, "real/q.rq"), formatted(QUERY));
    #[cfg(unix)]
    {
        write(dir, "real/l.rq", &format!("{BOM}{QUERY}"));
        std::os::unix::fs::symlink("real/l.rq", dir.join("link.rq")).unwrap();
        expect(dir, &["--write", "link.rq"], None, 0);
        assert!(
            std::fs::symlink_metadata(dir.join("link.rq"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(read(dir, "real/l.rq"), formatted(QUERY));
    }
    // --write needs files
    let o = expect(dir, &["--write"], Some(QUERY), 2);
    assert!(stderr(&o).contains("--write needs file paths"));
}

#[test]
fn check_list_and_diff() {
    let d = tempdir();
    let dir = d.path();
    let pretty = formatted(QUERY);
    write(dir, "queries/a.rq", &format!("{BOM}{QUERY}"));
    write(dir, "queries/b.rq", &pretty);
    let o = expect(dir, &["--check", "queries/"], None, 1);
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        "Checking formatting...\n\
         [warn] queries/a.rq\n\
         [warn] Code style issues found in the above file. Run sparkles fmt --write to fix.\n"
    );
    let o = expect(dir, &["-l", "queries"], None, 1);
    assert_eq!(stdout(&o), "queries/a.rq\n");
    let o = expect(dir, &["--check", "--diff", "queries"], None, 1);
    let diff = stdout(&o);
    assert!(
        diff.starts_with("--- a/queries/a.rq\n+++ b/queries/a.rq\n@@ "),
        "{diff}"
    );
    assert!(diff.contains(&format!("\n-{BOM}PREFIX")));

    // a syntax error: exit 2, every file still reported
    write(dir, "queries/bad.rq", BAD);
    let o = expect(dir, &["--check", "queries"], None, 2);
    let err = stderr(&o);
    assert!(err.contains("[warn] queries/a.rq\n"), "{err}");
    assert!(
        err.contains("\nqueries/bad.rq:3:14: error: SPARQL syntax error: "),
        "{err}"
    );
    assert!(err.ends_with(
        "[warn] Code style issues found in the above file. Run sparkles fmt --write to fix.\n"
    ));
    expect(dir, &["-l", "queries"], None, 2);

    // formatted files only
    std::fs::remove_file(dir.join("queries/bad.rq")).unwrap();
    std::fs::remove_file(dir.join("queries/a.rq")).unwrap();
    let o = expect(dir, &["--check", "queries"], None, 0);
    assert_eq!(
        stderr(&o),
        "Checking formatting...\nAll matched files are formatted.\n"
    );
    let o = expect(dir, &["-l", "queries"], None, 0);
    assert_eq!(stdout(&o), "");

    // the modes exclude each other, and --diff goes with --check
    for args in [
        &["--check", "--write", "queries"][..],
        &["-l", "--write", "queries"],
        &["--check", "-l", "queries"],
        &["--diff", "queries"],
    ] {
        expect(dir, args, None, 2);
    }
}

#[test]
fn many_files_in_parallel_keep_their_order() {
    let d = tempdir();
    let dir = d.path();
    let mut expected = String::new();
    let mut printed = String::new();
    for i in 0..40 {
        let name = format!("q/{i:02}.rq");
        let text = format!("{BOM}ASK {{ ?s <http://example.org/p{i}> ?o }}\n");
        write(dir, &name, &text);
        expected.push_str(&format!("{name}\n"));
        printed.push_str(&formatted(&text));
    }
    let o = expect(dir, &["-l", "--threads", "4", "q"], None, 1);
    assert_eq!(stdout(&o), expected);
    let o = expect(dir, &["q"], None, 0);
    assert_eq!(stdout(&o), printed);
}

#[test]
fn walks_and_explicit_paths() {
    let d = tempdir();
    let dir = d.path();
    let unformatted = format!("{BOM}{QUERY}");
    for f in [
        "w/a.rq",
        "w/u.ru",
        "w/q.sparql",
        "w/.hidden/h.rq",
        "w/node_modules/n.rq",
        "w/target/t.rq",
        "w/.git/g.rq",
    ] {
        write(dir, f, &unformatted);
    }
    // never formatted in walks: unimplemented languages, RDF/XML, compressed and other
    // files (each would be an error if given explicitly)
    for f in [
        "w/d.ttl",
        "w/d.trig",
        "w/d.nt",
        "w/d.nq",
        "w/d.jsonld",
        "w/x.rdf",
        "w/x.owl",
        "w/d.ttl.gz",
        "w/d.json",
        "w/d.n3",
        "w/notes.txt",
    ] {
        write(dir, f, "garbage\n");
    }
    let o = expect(dir, &["-l", "w"], None, 1);
    assert_eq!(stdout(&o), "w/.hidden/h.rq\nw/a.rq\nw/q.sparql\nw/u.ru\n");
    // "." shows the paths without "./"
    let o = expect(&dir.join("w"), &["-l", "."], None, 1);
    assert_eq!(stdout(&o), ".hidden/h.rq\na.rq\nq.sparql\nu.ru\n");
    // a file named twice is formatted once
    let o = expect(dir, &["-l", "w/a.rq", "w", "./w/a.rq"], None, 1);
    assert_eq!(stdout(&o).matches("a.rq").count(), 1, "{}", stdout(&o));

    // explicit paths: by extension, else by content
    for (file, message) in [
        (
            "w/d.ttl",
            "w/d.ttl: error: turtle formatting is not available yet",
        ),
        (
            "w/d.trig",
            "w/d.trig: error: trig formatting is not available yet",
        ),
        (
            "w/d.nt",
            "w/d.nt: error: ntriples formatting is not available yet",
        ),
        (
            "w/d.jsonld",
            "w/d.jsonld: error: jsonld formatting is not available yet",
        ),
        (
            "w/x.rdf",
            "w/x.rdf: error: RDF/XML formatting is not supported; convert to Turtle to format",
        ),
        (
            "w/d.ttl.gz",
            "w/d.ttl.gz: error: compressed input: decompress first",
        ),
        (
            "w/d.json",
            "w/d.json: error: cannot tell the language of a .json file; use --language",
        ),
        ("missing.rq", "missing.rq: error: No such file or directory"),
    ] {
        let o = expect(dir, &[file], None, 2);
        assert_eq!(stderr(&o), format!("{message}\n"));
        assert_eq!(stdout(&o), "");
    }
    let o = expect(dir, &["--language", "turtle", "w/a.rq"], None, 2);
    assert_eq!(
        stderr(&o),
        "w/a.rq: error: turtle formatting is not available yet\n"
    );
    // an unknown extension: the content decides
    write(dir, "query.txt", QUERY);
    let o = expect(dir, &["query.txt"], None, 0);
    assert_eq!(stdout(&o), formatted(QUERY));
    let o = expect(dir, &["--language", "sparql", "w/d.json"], None, 2);
    assert!(stderr(&o).starts_with("w/d.json:2:1: error: SPARQL syntax error: "));
    // a directory with nothing to format
    std::fs::create_dir(dir.join("empty")).unwrap();
    let o = expect(dir, &["--check", "empty"], None, 2);
    assert!(stderr(&o).contains("empty: error: no files to format"));
}

#[test]
fn ignore_files() {
    let d = tempdir();
    let dir = d.path();
    let unformatted = format!("{BOM}{QUERY}");
    for f in [
        "q/a.rq",
        "q/gen/g.rq",
        "q/skip/s.rq",
        "q/one.rq",
        "q/vendor/v.rq",
    ] {
        write(dir, f, &unformatted);
    }
    write(dir, "q/.gitignore", "gen/\n");
    write(dir, ".sparklesfmtignore", "skip/\none.rq\n");
    let o = expect(dir, &["-l", "q"], None, 1);
    assert_eq!(stdout(&o), "q/a.rq\nq/vendor/v.rq\n");
    // explicit files: .gitignore does not apply, the ignore file does (silently)
    let o = expect(
        dir,
        &["-l", "q/gen/g.rq", "q/one.rq", "q/skip/s.rq"],
        None,
        1,
    );
    assert_eq!(stdout(&o), "q/gen/g.rq\n");
    assert_eq!(stderr(&o), "");
    let o = expect(dir, &["--write", "q/one.rq"], None, 0);
    assert_eq!(stderr(&o), "");
    assert_eq!(read(dir, "q/one.rq"), unformatted);
    // .gitignore applies outside a git repository too, and inside one
    std::fs::create_dir(dir.join(".git")).unwrap();
    let o = expect(dir, &["-l", "."], None, 1);
    assert_eq!(stdout(&o), "q/a.rq\nq/vendor/v.rq\n");
    // --ignore-path replaces ./.sparklesfmtignore
    write(dir, "other-ignore", "vendor/\n");
    let o = expect(dir, &["-l", "--ignore-path", "other-ignore", "q"], None, 1);
    assert_eq!(stdout(&o), "q/a.rq\nq/one.rq\nq/skip/s.rq\n");
    let o = expect(dir, &["-l", "--ignore-path", "nope", "q"], None, 2);
    assert_eq!(stderr(&o), "nope: error: no such ignore file\n");
    // the ignore file is found in the current directory, not above it
    let o = expect(&dir.join("q"), &["-l", "."], None, 1);
    assert_eq!(stdout(&o), "a.rq\none.rq\nskip/s.rq\nvendor/v.rq\n");
}

#[test]
fn stdin_and_stdin_filepath() {
    let d = tempdir();
    let dir = d.path();
    let unformatted = format!("{BOM}{QUERY}");
    // without a name, the content decides
    let o = expect(dir, &[], Some(&unformatted), 0);
    assert_eq!(stdout(&o), formatted(QUERY));
    let o = expect(dir, &["--check"], Some(&unformatted), 1);
    assert!(stderr(&o).contains("[warn] <stdin>\n"));
    let o = expect(dir, &["-l"], Some(&unformatted), 1);
    assert_eq!(stdout(&o), "<stdin>\n");
    let o = expect(dir, &[], Some(BAD), 2);
    assert!(
        stderr(&o).starts_with("<stdin>:3:14: error: SPARQL syntax error: "),
        "{}",
        stderr(&o)
    );
    let o = expect(dir, &[], Some("<a> <b> <c> .\n"), 2);
    assert_eq!(
        stderr(&o),
        "<stdin>: error: turtle formatting is not available yet\n"
    );
    let o = expect(dir, &[], Some("<?xml version=\"1.0\"?>\n<rdf:RDF/>\n"), 2);
    assert_eq!(
        stderr(&o),
        "<stdin>: error: RDF/XML formatting is not supported; convert to Turtle to format\n"
    );
    let o = expect(dir, &[], Some("# nothing\n"), 2);
    assert_eq!(
        stderr(&o),
        "<stdin>: error: cannot tell the language; use --language\n"
    );

    // the name: language, messages, config and ignore files
    let o = expect(
        dir,
        &["--stdin-filepath", "q/x.rq", "--check"],
        Some(BAD),
        2,
    );
    assert!(stderr(&o).contains("\nq/x.rq:3:14: error: "));
    let o = expect(dir, &["--stdin-filepath", "data.ttl"], Some(QUERY), 2);
    assert_eq!(
        stderr(&o),
        "data.ttl: error: turtle formatting is not available yet\n"
    );
    let o = expect(dir, &["--stdin-filepath", "x.owl"], Some(QUERY), 2);
    assert!(stderr(&o).contains("RDF/XML formatting is not supported"));
    write(dir, "sub/.sparklesfmt.toml", "line-widht = 80\n");
    let o = expect(dir, &["--stdin-filepath", "sub/q.rq"], Some(QUERY), 2);
    assert_eq!(
        stderr(&o),
        "sub/.sparklesfmt.toml: error: line-widht: unknown option\n"
    );
    expect(dir, &["--stdin-filepath", "q.rq"], Some(QUERY), 0);
    // an ignored name passes the input through unchanged, even a broken one
    write(dir, ".sparklesfmtignore", "vendor/\n");
    let o = expect(dir, &["--stdin-filepath", "vendor/x.rq"], Some(BAD), 0);
    assert_eq!(stdout(&o), BAD);
    let o = expect(
        dir,
        &["--stdin-filepath", "vendor/x.rq", "--check"],
        Some(BAD),
        0,
    );
    assert_eq!(stdout(&o), "");
    // a name goes with stdin only
    write(dir, "a.rq", QUERY);
    expect(dir, &["--stdin-filepath", "x.rq", "a.rq"], None, 2);
}

/// The `option-not-implemented` warning of `align-values = true` shows which options a
/// file got.
const ALIGN: &str = "warning: align-values is not implemented yet and has no effect";

#[test]
fn config_discovery() {
    let d = tempdir();
    let dir = d.path();
    let pretty = formatted(QUERY);
    for f in ["top.rq", "sub/mid.rq", "sub/deep/low.rq", "other/o.rq"] {
        write(dir, f, &pretty);
    }
    write(dir, ".sparklesfmt.toml", "align-values = true\n");
    // the nearest file wins and is not merged with the one above it
    write(dir, "sub/sparklesfmt.toml", "line-width = 80\n");
    let o = expect(dir, &["--check", "sub/deep/low.rq"], None, 0);
    assert!(!stderr(&o).contains(ALIGN), "{}", stderr(&o));
    let o = expect(dir, &["--check", "top.rq", "other/o.rq"], None, 0);
    // said once for the whole run
    assert_eq!(stderr(&o).matches(ALIGN).count(), 1, "{}", stderr(&o));
    // from a subdirectory, the files above still apply
    let o = expect(&dir.join("other"), &["--check", "o.rq"], None, 0);
    assert!(stderr(&o).contains(ALIGN));
    // flags override the file
    let o = expect(dir, &["--check", "--no-align-values", "top.rq"], None, 0);
    assert!(!stderr(&o).contains(ALIGN));
    // --config: one file for every input, no discovery
    write(dir, "conf/style.toml", "align-values = true\n");
    let o = expect(
        dir,
        &["--check", "--config", "conf/style.toml", "sub/mid.rq"],
        None,
        0,
    );
    assert!(stderr(&o).contains(ALIGN));
    // --no-config: the defaults, even under a broken file
    write(dir, "sub/sparklesfmt.toml", "line-width = 1000\n");
    expect(dir, &["--check", "sub/mid.rq"], None, 2);
    let o = expect(
        dir,
        &["--check", "--no-config", "sub/mid.rq", "top.rq"],
        None,
        0,
    );
    assert!(!stderr(&o).contains(ALIGN));
    // a broken file fails the files under it, is reported once, and the others go on
    let o = expect(
        dir,
        &["-l", "sub/mid.rq", "sub/deep/low.rq", "top.rq"],
        None,
        2,
    );
    assert_eq!(
        stderr(&o),
        format!(
            "sub/sparklesfmt.toml: error: line-width: expected an integer from 40 to 400, got 1000\n{ALIGN}\n"
        )
    );
    // the dotfile wins over sparklesfmt.toml in one directory: its indent width applies (the
    // file, formatted with 2 spaces, now needs changes) instead of the broken file's error
    write(dir, "sub/.sparklesfmt.toml", "indent-width = 4\n");
    expect(dir, &["--check", "sub/mid.rq"], None, 1);
    expect(dir, &["--config", "nope.toml", "top.rq"], None, 2);
    let o = expect(dir, &["--config", "nope.toml", "top.rq"], None, 2);
    assert_eq!(stderr(&o), "nope.toml: error: No such file or directory\n");
    expect(
        dir,
        &["--config", "a.toml", "--no-config", "top.rq"],
        None,
        2,
    );
}

#[test]
fn config_errors_name_the_key_and_the_file() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "q/q.rq", QUERY);
    for (toml, message) in [
        (
            "lineWidth = 80\n",
            "lineWidth: unknown option (did you mean line-width?)",
        ),
        ("colour = true\n", "colour: unknown option"),
        (
            "quote-style = \"singel\"\n",
            "quote-style: expected \"double\" or \"preserve\", got \"singel\"",
        ),
        (
            "operator-position = \"Leading\"\n",
            "operator-position: expected \"leading\" or \"trailing\", got \"Leading\"",
        ),
        (
            "prefix-groups = [[\"rdf\", \"rdfs\"], [\"owl\", \"rdf\"]]\n",
            "prefix-groups: \"rdf\" is in more than one place",
        ),
        (
            "prefix-groups = [\"rdf\"]\n",
            "prefix-groups: expected an array of arrays of prefix labels, such as [[\"rdf\", \"rdfs\", \"xsd\"]]",
        ),
        (
            "indent-width = 9\n",
            "indent-width: expected an integer from 1 to 8, got 9",
        ),
        (
            "sort = \"no\"\n",
            "sort: expected true or false, got a string",
        ),
    ] {
        write(dir, "q/.sparklesfmt.toml", toml);
        let o = expect(dir, &["q/q.rq"], None, 2);
        assert_eq!(
            stderr(&o),
            format!("q/.sparklesfmt.toml: error: {message}\n"),
            "{toml}"
        );
        assert_eq!(stdout(&o), "");
        // the same file through --config stops the run before any file
        let o = expect(dir, &["--config", "q/.sparklesfmt.toml", "q/q.rq"], None, 2);
        assert_eq!(
            stderr(&o),
            format!("q/.sparklesfmt.toml: error: {message}\n")
        );
    }
    write(dir, "q/.sparklesfmt.toml", "line-width = 80\nsort = \n");
    let o = expect(dir, &["q/q.rq"], None, 2);
    assert!(
        stderr(&o).starts_with("q/.sparklesfmt.toml:2:"),
        "{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains(": error: invalid TOML: "));
    // the same checks on the flags
    for (args, message) in [
        (
            &["--line-width", "20"][..],
            "error: --line-width: expected an integer from 40 to 400, got 20",
        ),
        (
            &["--quote-style", "single"],
            "error: --quote-style: expected \"double\" or \"preserve\", got \"single\"",
        ),
        (
            &["--prefix-group", "rdf,rdfs", "--prefix-group", "rdf"],
            "error: --prefix-group: \"rdf\" is in more than one place",
        ),
    ] {
        let o = expect(dir, args, Some(QUERY), 2);
        assert_eq!(stderr(&o), format!("{message}\n"));
    }
}

/// With the test-only fault switch the printer drops a token: the check refuses the
/// output, `--write` leaves the file alone, and the endpoint answers 422.
#[test]
fn documents_over_max_bytes_are_refused() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "big.rq", QUERY);
    let o = expect(dir, &["--max-bytes", "16", "big.rq"], None, 2);
    assert!(
        stderr(&o).contains(&format!(
            "big.rq: error: {} bytes is more than --max-bytes (16) allows to format in memory",
            QUERY.len()
        )),
        "{}",
        stderr(&o)
    );
    // stdin too
    let o = expect(
        dir,
        &["--max-bytes=16", "--language", "sparql"],
        Some(QUERY),
        2,
    );
    assert!(stderr(&o).contains("--max-bytes"), "{}", stderr(&o));
    expect(dir, &["--max-bytes", "1KiB", "big.rq"], None, 0);
    expect(dir, &["--max-bytes", "lots", "big.rq"], None, 2);
}

#[test]
fn the_language_server_is_not_available_yet() {
    let o = Command::new(BIN).args(["lsp", "--stdio"]).output().unwrap();
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("sparkles lsp is not available yet"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn refused_output_is_never_written() {
    let d = tempdir();
    let dir = d.path();
    let unformatted = format!("{BOM}{QUERY}");
    write(dir, "x.rq", &unformatted);
    let run = |args: &[&str]| {
        Command::new(BIN)
            .arg("fmt")
            .args(args)
            .current_dir(dir)
            .env("SPARKLES_FMT_FAULT", "drop-token")
            .output()
            .unwrap()
    };
    for args in [&["--write", "x.rq"][..], &["--check", "x.rq"], &["x.rq"]] {
        let o = run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains(
                "x.rq: error: formatter refused its own output (algebra differs); input left unchanged; please report"
            ),
            "{}",
            stderr(&o)
        );
        assert_eq!(stdout(&o), "");
        assert_eq!(read(dir, "x.rq"), unformatted);
    }

    let server = Server::start(dir, &[("SPARKLES_FMT_FAULT", "drop-token")]);
    let (status, body) = server.post(
        "/$/format",
        "application/json",
        r#"{"text": "SELECT ?s WHERE { ?s ?p ?o }"}"#,
    );
    assert_eq!(status, 422, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["code"], "unsafe-format", "{j}");
    assert!(
        j["error"].as_str().unwrap().contains("algebra differs"),
        "{j}"
    );
    assert!(j["requestId"].is_string(), "{j}");
}

/// The endpoint on a real server without auth, for an anonymous caller: both body
/// forms and RDF/XML refused.
#[test]
fn the_endpoint_on_a_running_server() {
    let d = tempdir();
    let server = Server::start(d.path(), &[]);
    let (status, body) = server.post(
        "/$/format",
        "application/json",
        r#"{"text": "ASK {}", "language": "sparql", "cursorOffset": 3}"#,
    );
    assert_eq!(status, 200, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["language"], "sparql");
    assert_eq!(j["text"], formatted("ASK {}"));
    let (status, body) = server.post("/$/format", "application/sparql-query", QUERY);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, formatted(QUERY));
    let (status, _) = server.post("/$/format", "application/rdf+xml", "<rdf:RDF/>");
    assert_eq!(status, 415);
}

/// A server on a temporary data directory, stopped when dropped.
struct Server {
    child: std::process::Child,
    port: u16,
}

impl Server {
    fn start(data: &Path, env: &[(&str, &str)]) -> Server {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut c = Command::new(BIN);
        c.args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(data.join("data"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        let s = Server {
            child: c.spawn().unwrap(),
            port,
        };
        let t0 = std::time::Instant::now();
        while s.request("GET", "/$/ping", "", "").map(|r| r.0) != Some(200) {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    fn request(&self, method: &str, path: &str, ct: &str, body: &str) -> Option<(u16, String)> {
        use std::io::Read;
        let mut c = std::net::TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        write!(
            c,
            "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .ok()?;
        let mut buf = String::new();
        c.read_to_string(&mut buf).ok()?;
        let (head, body) = buf.split_once("\r\n\r\n")?;
        let status = head.split(' ').nth(1)?.parse().ok()?;
        Some((status, body.to_string()))
    }

    fn post(&self, path: &str, ct: &str, body: &str) -> (u16, String) {
        self.request("POST", path, ct, body)
            .unwrap_or_else(|| panic!("POST {path}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn reformats_messy_queries() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "q.rq", "select*{?s ?p ?o}");
    expect(dir, &["--check", "q.rq"], None, 1);
    expect(dir, &["--write", "q.rq"], None, 0);
    expect(dir, &["--check", "q.rq"], None, 0);
    assert_eq!(read(dir, "q.rq"), "SELECT *\nWHERE {\n  ?s ?p ?o .\n}\n");
}
