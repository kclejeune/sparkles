//! `sparkles fmt` on N-Triples and N-Quads, run as the real binary: the streaming modes
//! (print, `--check`, `-l`, `--write` with its kept modification time), stdin, sorting
//! that spills, `--canonicalize`, `--diff` in memory under `--max-bytes`, errors, and the
//! endpoint on a running server.

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// The N-Quads example of the formatter's documentation, and its outputs.
const L1: &str = "# export 2026-09-30\n\
<http://example.org/b>   <http://example.org/p> \"café\"@FR <http://example.org/g> .\n\
_:x <http://example.org/p> <http://example.org/o> .\n\
<http://example.org/a> <http://example.org/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#string> <http://example.org/g> .   # first\n\
_:x <http://example.org/p> <http://example.org/o> .\n";
const L1_OUT: &str = "# export 2026-09-30\n\n\
<http://example.org/b> <http://example.org/p> \"café\"@fr <http://example.org/g> .\n\
_:x <http://example.org/p> <http://example.org/o> .\n\
<http://example.org/a> <http://example.org/p> \"1\" <http://example.org/g> . # first\n\
_:x <http://example.org/p> <http://example.org/o> .\n";
const L1_SORTED: &str = "# export 2026-09-30\n\n\
<http://example.org/a> <http://example.org/p> \"1\" <http://example.org/g> . # first\n\
<http://example.org/b> <http://example.org/p> \"café\"@fr <http://example.org/g> .\n\
_:x <http://example.org/p> <http://example.org/o> .\n";
/// A syntax error at 2:24 (the object is missing).
const BAD: &str = "<http://e/s> <http://e/p> <http://e/o> .\n<http://e/s> <http://e/p> .\n";

/// `sparkles fmt ARGS` in `dir`, with stdin and environment variables.
fn fmt_env(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &Path)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("fmt")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
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
fn expect(dir: &Path, args: &[&str], stdin: Option<&str>, code: i32) -> Output {
    expect_env(dir, args, stdin, &[], code)
}

#[track_caller]
fn expect_env(
    dir: &Path,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &Path)],
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

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn prints_checks_and_writes_streams() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    write(dir, "l1.nq", L1);
    let o = expect(dir, &["l1.nq"], None, 0);
    assert_eq!(stdout(&o), L1_OUT);
    assert_eq!(read(dir, "l1.nq"), L1);
    let o = expect(dir, &["--sort", "l1.nq"], None, 0);
    assert_eq!(stdout(&o), L1_SORTED);

    let o = expect(dir, &["--check", "l1.nq"], None, 1);
    assert_eq!(
        stderr(&o),
        "Checking formatting...\n\
         [warn] l1.nq\n\
         [warn] Code style issues found in the above file. Run sparkles fmt --write to fix.\n"
    );
    let o = expect(dir, &["-l", "l1.nq"], None, 1);
    assert_eq!(stdout(&o), "l1.nq\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("l1.nq"), std::fs::Permissions::from_mode(0o640))
            .unwrap();
    }
    let before = std::fs::metadata(dir.join("l1.nq")).unwrap();
    let o = expect(dir, &["--write", "l1.nq"], None, 0);
    assert_eq!(stdout(&o), "");
    assert_eq!(read(dir, "l1.nq"), L1_OUT);
    let after = std::fs::metadata(dir.join("l1.nq")).unwrap();
    assert_eq!(before.permissions(), after.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(before.ino(), after.ino());
    }
    assert_eq!(names(dir), ["l1.nq"], "a temporary file was left behind");

    // formatted: --write leaves the file alone, with its modification time
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(dir.join("l1.nq"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    expect(dir, &["--write", "l1.nq"], None, 0);
    assert_eq!(
        std::fs::metadata(dir.join("l1.nq"))
            .unwrap()
            .modified()
            .unwrap(),
        old
    );
    assert_eq!(names(dir), ["l1.nq"]);
    expect(dir, &["--check", "l1.nq"], None, 0);
    expect(dir, &["-l", "l1.nq"], None, 0);
    // sorted, it changes
    expect(dir, &["--check", "--sort", "l1.nq"], None, 1);
    expect(dir, &["--write", "--sort", "l1.nq"], None, 0);
    assert_eq!(read(dir, "l1.nq"), L1_SORTED);
}

#[test]
fn stdin_streams_when_the_language_is_named() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let o = expect(dir, &["--language", "nquads"], Some(L1), 0);
    assert_eq!(stdout(&o), L1_OUT);
    let o = expect(dir, &["--stdin-filepath", "x.nq", "--sort"], Some(L1), 0);
    assert_eq!(stdout(&o), L1_SORTED);
    expect(dir, &["--check", "--language", "nquads"], Some(L1), 1);
    expect(dir, &["--check", "--language", "nquads"], Some(L1_OUT), 0);
    // sniffed N-Quads is read whole and formatted the same
    let o = expect(dir, &[], Some(L1), 0);
    assert_eq!(stdout(&o), L1_OUT);
    // an ignored --stdin-filepath passes stdin through
    write(dir, ".sparklesfmtignore", "*.nq\n");
    let o = expect(dir, &["--stdin-filepath", "x.nq"], Some(L1), 0);
    assert_eq!(stdout(&o), L1);
    // N-Triples has no graph names
    let o = expect(dir, &["--language", "ntriples"], Some(L1), 2);
    assert!(stderr(&o).starts_with("<stdin>:2:"), "{}", stderr(&o));
}

#[test]
fn errors_leave_files_alone() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    write(dir, "bad.nt", BAD);
    write(dir, "ok.nq", L1);
    let o = expect(dir, &["--write", "bad.nt", "ok.nq"], None, 2);
    let err = stderr(&o);
    assert!(err.contains("bad.nt:2:"), "{err}");
    assert!(err.contains(": error: N-Triples syntax error: "), "{err}");
    assert_eq!(read(dir, "bad.nt"), BAD);
    assert_eq!(read(dir, "ok.nq"), L1_OUT);
    assert_eq!(
        names(dir),
        ["bad.nt", "ok.nq"],
        "a temporary file was left behind"
    );
    let o = expect(dir, &["--check", "bad.nt", "ok.nq"], None, 2);
    assert!(stderr(&o).contains("bad.nt:2:"));
    // printing stops at the error (in a small file, before anything is printed)
    let o = expect(dir, &["bad.nt"], None, 2);
    assert_eq!(stdout(&o), "");
    // canonicalizing has a size limit
    let o = expect(
        dir,
        &["--canonicalize", "--max-canonicalize-quads", "2", "ok.nq"],
        None,
        2,
    );
    assert!(
        stderr(&o).contains("ok.nq: error: the input is too large"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn canonicalize_relabels_and_says_what_it_dropped() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    write(dir, "a.nq", L1);
    write(
        dir,
        "b.nq",
        "_:other <http://example.org/p> <http://example.org/o> .\n\
         <http://example.org/a> <http://example.org/p> \"1\" <http://example.org/g> .\n\
         <http://example.org/b> <http://example.org/p> \"café\"@fr <http://example.org/g> .\n",
    );
    let a = expect(dir, &["--canonicalize", "a.nq"], None, 0);
    let b = expect(dir, &["--canonicalize", "b.nq"], None, 0);
    assert_eq!(stdout(&a), stdout(&b));
    assert!(stdout(&a).contains("_:c14n0 "), "{}", stdout(&a));
    assert_eq!(
        stderr(&a),
        "a.nq: warning: canonicalize dropped 2 comments\n"
    );
    assert_eq!(stderr(&b), "");
    expect(dir, &["--write", "--canonicalize", "b.nq"], None, 0);
    expect(dir, &["--check", "--canonicalize", "b.nq"], None, 0);
}

#[test]
fn sorting_spills_to_the_temporary_directory() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let spill = dir.join("tmp");
    std::fs::create_dir(&spill).unwrap();
    let mut text = String::new();
    for i in 0..20_000u64 {
        let k = i.wrapping_mul(7919) % 10_007;
        text.push_str(&format!(
            "<http://example.org/s{k}> <http://example.org/p>   \"v{i}\" .\n"
        ));
    }
    write(dir, "big.nt", &text);
    let env = [("TMPDIR", spill.as_path())];
    let in_memory = expect_env(dir, &["--sort", "big.nt"], None, &env, 0);
    let spilled = expect_env(
        dir,
        &["--sort", "--sort-memory", "16KiB", "big.nt"],
        None,
        &env,
        0,
    );
    assert_eq!(stdout(&spilled), stdout(&in_memory));
    assert_eq!(stdout(&spilled).lines().count(), 20_000);
    assert_eq!(
        names(&spill),
        Vec::<String>::new(),
        "spill files left behind"
    );
    // it does spill: a temporary directory that is not there fails the run
    let missing = dir.join("missing");
    let o = expect_env(
        dir,
        &["--sort", "--sort-memory", "16KiB", "big.nt"],
        None,
        &[("TMPDIR", missing.as_path())],
        2,
    );
    assert!(stderr(&o).contains("big.nt: error: "), "{}", stderr(&o));
}

#[test]
fn diff_formats_in_memory_under_max_bytes() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    write(dir, "l1.nq", L1);
    let o = expect(dir, &["--check", "--diff", "l1.nq"], None, 1);
    assert!(
        stdout(&o).starts_with("--- a/l1.nq\n+++ b/l1.nq\n@@ "),
        "{}",
        stdout(&o)
    );
    // streaming has no size limit; --diff has
    expect(dir, &["--max-bytes", "16", "l1.nq"], None, 0);
    expect(dir, &["--max-bytes", "16", "--check", "l1.nq"], None, 1);
    let o = expect(
        dir,
        &["--max-bytes", "16", "--check", "--diff", "l1.nq"],
        None,
        2,
    );
    assert!(stderr(&o).contains("--max-bytes"), "{}", stderr(&o));
}

#[test]
fn files_of_every_language_print_in_order() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let mut expected = String::new();
    for i in 0..12 {
        if i % 3 == 0 {
            write(dir, &format!("f/{i:02}.rq"), "ASK {}\n");
            expected.push_str("ASK {}\n");
        } else {
            write(
                dir,
                &format!("f/{i:02}.nt"),
                &format!("<http://e/s{i}>  <http://e/p> \"{i}\" .\n"),
            );
            expected.push_str(&format!("<http://e/s{i}> <http://e/p> \"{i}\" .\n"));
        }
    }
    let o = expect(dir, &["--threads", "4", "f"], None, 0);
    assert_eq!(stdout(&o), expected);
}

/// A server on a temporary data directory, stopped when dropped.
struct Server {
    child: std::process::Child,
    port: u16,
}

impl Server {
    fn start(data: &Path) -> Server {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let s = Server {
            child: Command::new(BIN)
                .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
                .arg("--data")
                .arg(data.join("data"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_endpoint_formats_line_formats() {
    let d = tempfile::tempdir().unwrap();
    let server = Server::start(d.path());
    let (status, body) = server
        .request("POST", "/$/format", "application/n-quads", L1)
        .unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, L1_OUT);
    let req = serde_json::json!({ "text": L1, "language": "nquads", "options": { "sort": true } });
    let (status, body) = server
        .request("POST", "/$/format", "application/json", &req.to_string())
        .unwrap();
    assert_eq!(status, 200, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["text"], L1_SORTED);
    assert_eq!(j["language"], "nquads");
    let (status, body) = server
        .request("POST", "/$/format", "application/n-triples", BAD)
        .unwrap();
    assert_eq!(status, 400, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (j["code"].as_str(), j["line"].as_u64()),
        (Some("syntax"), Some(2)),
        "{j}"
    );
}
