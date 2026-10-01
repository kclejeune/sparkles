//! `sparkles fmt` on Turtle and TriG over `--max-bytes`, run as the real binary: they
//! stream statement by statement and print what formatting in memory prints (to stdout,
//! for `--check` and `-l`, and through `--write` with its kept permissions and
//! modification time), from files and from stdin (named or sniffed, read twice with
//! `prune-prefixes`); sorting and `--diff` keep the limit; errors, refused output and a
//! statement over the limit leave files alone and no temporary file behind.

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// `sparkles fmt ARGS` in `dir`, with stdin and environment variables.
fn fmt_env(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("fmt")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("TMPDIR", dir.join("tmp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    // a run that does not read stdin may close it early
    let _ = input.write_all(stdin.unwrap_or("").as_bytes());
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
fn expect_env(
    dir: &Path,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &str)],
    code: i32,
) -> Output {
    let o = fmt_env(dir, args, stdin, env);
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
fn expect(dir: &Path, args: &[&str], stdin: Option<&str>, code: i32) -> Output {
    expect_env(dir, args, stdin, &[], code)
}

fn write(dir: &Path, rel: &str, text: &str) {
    std::fs::write(dir.join(rel), text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The files in `dir` but the temporary directory.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "tmp")
        .collect();
    v.sort();
    v
}

fn tempdir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("tmp")).unwrap();
    d
}

/// A Turtle document of a few KiB: a header, directives mid-document, comments, blank
/// lines, a pragma, blank nodes, an unused prefix.
fn turtle() -> String {
    let mut s = String::from(
        "# a generated document\n\n@prefix ex: <http://example.org/> .\n@prefix unused: <http://unused.org/> .\n",
    );
    for i in 0..60 {
        match i % 6 {
            0 => s.push_str(&format!("ex:s{i}   ex:p   ex:o{i} ; ex:q \"{i}\" .\n")),
            1 => s.push_str(&format!(
                "\n# about {i}\n<http://example.org/t{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ex:C . # t\n"
            )),
            2 => s.push_str(&format!("_:b{i} ex:p [ ex:q {i} ; ex:r ( 1 2 ) ] .\n\n\n")),
            3 => s.push_str(&format!("PREFIX x{i}: <http://example.org/x{i}/>\nx{i}:a ex:p 'single' .\n")),
            4 => s.push_str(&format!(
                "# sparkles-fmt: ignore\nex:kept{i}   ex:p   ex:o .\n"
            )),
            _ => s.push_str(&format!("ex:s{i} ex:p ex:a, ex:b, ex:c . ex:s{i} ex:q 1 .\n")),
        }
    }
    s
}

/// A TriG document with one graph block of a few KiB.
fn trig() -> String {
    let mut s = String::from("PREFIX ex: <http://example.org/>\n\nGRAPH ex:g {\n");
    for i in 0..80 {
        s.push_str(&format!("  ex:s{i}   ex:p   ex:o{i} ;  ex:q {i}\n  .\n"));
    }
    s.push_str("}\nex:after ex:p 1 .\n");
    s
}

#[test]
fn large_files_stream_like_they_format_in_memory() {
    let d = tempdir();
    let dir = d.path();
    for (file, text) in [("doc.ttl", turtle()), ("doc.trig", trig())] {
        write(dir, file, &text);
        assert!(text.len() > 3000);
        // in memory, then streamed: the same
        let whole = stdout(&expect(dir, &[file], None, 0));
        let o = expect(dir, &["--max-bytes", "1KiB", file], None, 0);
        assert_eq!(stdout(&o), whole, "{file}");
        let pruned = stdout(&expect(dir, &["--prune-prefixes", file], None, 0));
        let o = expect(
            dir,
            &["--prune-prefixes", "--max-bytes", "1KiB", file],
            None,
            0,
        );
        assert_eq!(stdout(&o), pruned, "{file}");
        let o = expect(
            dir,
            &[
                "--turtle-layout",
                "conventional",
                "--max-bytes",
                "1KiB",
                file,
            ],
            None,
            0,
        );
        let conventional = expect(dir, &["--turtle-layout", "conventional", file], None, 0);
        assert_eq!(stdout(&o), stdout(&conventional), "{file}");

        expect(dir, &["--max-bytes", "1KiB", "--check", file], None, 1);
        let o = expect(dir, &["--max-bytes", "1KiB", "-l", file], None, 1);
        assert_eq!(stdout(&o), format!("{file}\n"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(file), std::fs::Permissions::from_mode(0o640))
                .unwrap();
        }
        let before = std::fs::metadata(dir.join(file)).unwrap();
        expect(dir, &["--max-bytes", "1KiB", "--write", file], None, 0);
        assert_eq!(read(dir, file), whole);
        let after = std::fs::metadata(dir.join(file)).unwrap();
        assert_eq!(before.permissions(), after.permissions());
        // formatted: --write leaves it alone, with its modification time
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(dir.join(file))
            .unwrap()
            .set_modified(old)
            .unwrap();
        expect(dir, &["--max-bytes", "1KiB", "--write", file], None, 0);
        let modified = std::fs::metadata(dir.join(file))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(modified, old);
        expect(dir, &["--max-bytes", "1KiB", "--check", file], None, 0);
    }
    assert_eq!(
        names(dir),
        ["doc.trig", "doc.ttl"],
        "a temporary file was left behind"
    );
    assert!(names(&dir.join("tmp")).is_empty());
}

#[test]
fn stdin_streams_past_max_bytes() {
    let d = tempdir();
    let dir = d.path();
    let text = turtle();
    let whole = stdout(&expect(dir, &["--language", "turtle"], Some(&text), 0));
    for args in [
        &["--language", "turtle", "--max-bytes", "1KiB"][..],
        &["--stdin-filepath", "x.ttl", "--max-bytes", "1KiB"],
        // sniffed from the bytes read
        &["--max-bytes", "1KiB"],
    ] {
        let o = expect(dir, args, Some(&text), 0);
        assert_eq!(stdout(&o), whole, "{args:?}");
    }
    // read twice: kept in a temporary file, which goes
    let pruned = stdout(&expect(
        dir,
        &["--language", "turtle", "--prune-prefixes"],
        Some(&text),
        0,
    ));
    let o = expect(
        dir,
        &[
            "--language",
            "turtle",
            "--prune-prefixes",
            "--max-bytes",
            "1KiB",
        ],
        Some(&text),
        0,
    );
    assert_eq!(stdout(&o), pruned);
    assert!(
        names(&dir.join("tmp")).is_empty(),
        "{:?}",
        names(&dir.join("tmp"))
    );
    expect(
        dir,
        &["--check", "--language", "turtle", "--max-bytes", "1KiB"],
        Some(&text),
        1,
    );
    expect(
        dir,
        &["--check", "--language", "turtle", "--max-bytes", "1KiB"],
        Some(&whole),
        0,
    );
    // an ignored --stdin-filepath passes stdin through
    write(dir, ".sparklesfmtignore", "*.ttl\n");
    let o = expect(
        dir,
        &["--stdin-filepath", "x.ttl", "--max-bytes", "1KiB"],
        Some(&text),
        0,
    );
    assert_eq!(stdout(&o), text);
    // SPARQL over the limit is refused, before reading it all
    let query = format!("SELECT * {{ {} }}", "?s ?p ?o . ".repeat(200));
    let o = expect(
        dir,
        &["--language", "sparql", "--max-bytes", "1KiB"],
        Some(&query),
        2,
    );
    assert!(
        stderr(&o).contains("<stdin>: error: the input is more than --max-bytes (1024)"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn sorting_and_diff_keep_the_limit() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "doc.ttl", &turtle());
    for args in [
        &["--sort", "--max-bytes", "1KiB", "doc.ttl"][..],
        &["--check", "--diff", "--max-bytes", "1KiB", "doc.ttl"],
    ] {
        let o = expect(dir, args, None, 2);
        let err = stderr(&o);
        assert!(
            err.contains("is more than --max-bytes (1024) allows to format in memory; sorting and --diff need it in memory"),
            "{args:?}: {err}"
        );
    }
    // under the limit, both work
    expect(dir, &["--sort", "doc.ttl"], None, 0);
    expect(dir, &["--check", "--diff", "doc.ttl"], None, 1);
}

#[test]
fn errors_leave_files_alone() {
    let d = tempdir();
    let dir = d.path();
    // a syntax error far into the file
    let mut bad = turtle();
    let line = bad.lines().count() + 1;
    bad.push_str("ex:s ex:p .\n");
    write(dir, "bad.ttl", &bad);
    let o = expect(dir, &["--max-bytes", "1KiB", "--write", "bad.ttl"], None, 2);
    let err = stderr(&o);
    assert!(
        err.contains(&format!("bad.ttl:{line}:")) && err.contains("Turtle syntax error"),
        "{err}"
    );
    assert_eq!(read(dir, "bad.ttl"), bad);
    // the same error as in memory
    let whole = expect(dir, &["bad.ttl"], None, 2);
    assert_eq!(stderr(&whole), err);
    // refused output
    let text = turtle();
    write(dir, "ok.ttl", &text);
    let o = expect_env(
        dir,
        &["--max-bytes", "1KiB", "--write", "ok.ttl"],
        None,
        &[("SPARKLES_FMT_FAULT", "drop-token")],
        2,
    );
    assert!(
        stderr(&o).contains("ok.ttl: error: formatter refused its own output"),
        "{}",
        stderr(&o)
    );
    assert_eq!(read(dir, "ok.ttl"), text);
    // a statement over the limit
    let big = format!(
        "<http://e/s> <http://e/p> {} .\n",
        (0..400)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    write(dir, "big.ttl", &big);
    let o = expect(dir, &["--max-bytes", "1KiB", "--write", "big.ttl"], None, 2);
    assert!(
        stderr(&o).contains("big.ttl: error: a statement is larger than --max-bytes (1024)"),
        "{}",
        stderr(&o)
    );
    assert_eq!(read(dir, "big.ttl"), big);
    assert_eq!(
        names(dir),
        ["bad.ttl", "big.ttl", "ok.ttl"],
        "a temporary file was left behind"
    );
}
