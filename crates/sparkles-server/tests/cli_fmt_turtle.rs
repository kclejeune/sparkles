//! `sparkles fmt` on Turtle and TriG, run as the real binary in temporary directories:
//! walks pick `.ttl` and `.trig`, `--check` exit codes, positioned syntax errors, the
//! nearest `.sparklesfmt.toml`, stdin by name or by content, and output the safety checks
//! refuse (never written; `POST /$/format` answers 422).

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const TURTLE: &str = "@prefix ex: <http://example.org/> .\nex:s ex:p 'o' ; a ex:C .\n";
const TURTLE_OUT: &str =
    "PREFIX ex: <http://example.org/>\n\nex:s\n  a ex:C ;\n  ex:p \"o\" ;\n.\n";
const TRIG: &str = "@prefix ex: <http://example.org/> .\nex:g { ex:s ex:p ex:o }\n";
const TRIG_OUT: &str = "PREFIX ex: <http://example.org/>\n\nGRAPH ex:g {\n  ex:s ex:p ex:o .\n}\n";
/// A syntax error at 2:9.
const BAD: &str = "@prefix ex: <http://example.org/> .\nex:s ex:p .\n";

/// The T1 example of the formatter's golden files, and its output under
/// `directive-style = "turtle"` with prefix groups.
const T1: &str = include_str!("../../sparkles-fmt/tests/golden/turtle/t1.in.ttl");
const T1_GROUPS: &str = include_str!("../../sparkles-fmt/tests/golden/turtle/t1.groups.out.ttl");
const T1_GROUPS_TOML: &str = include_str!("../../sparkles-fmt/tests/golden/turtle/t1.groups.toml");

/// `sparkles fmt ARGS` in `dir`, with stdin and extra environment variables.
fn fmt_in(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
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
    let o = fmt_in(dir, args, stdin, &[]);
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

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn walks_take_turtle_and_trig() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "w/a.ttl", TURTLE);
    write(dir, "w/b.trig", TRIG);
    write(dir, "w/c.TTL", TURTLE_OUT);
    write(dir, "w/d.turtle", TURTLE);
    let o = expect(dir, &["-l", "w"], None, 1);
    assert_eq!(stdout(&o), "w/a.ttl\nw/b.trig\nw/d.turtle\n");
    let o = expect(dir, &["--write", "w"], None, 0);
    assert_eq!(stdout(&o), "");
    assert_eq!(read(dir, "w/a.ttl"), TURTLE_OUT);
    assert_eq!(read(dir, "w/b.trig"), TRIG_OUT);
    assert_eq!(read(dir, "w/d.turtle"), TURTLE_OUT);
    expect(dir, &["--check", "w"], None, 0);
}

#[test]
fn check_exit_codes_and_syntax_errors() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "ok.ttl", TURTLE_OUT);
    write(dir, "todo.ttl", TURTLE);
    write(dir, "bad.ttl", BAD);
    write(dir, "bad.trig", "GRAPH <g> { <a> <b> }\n");
    expect(dir, &["--check", "ok.ttl"], None, 0);
    let o = expect(dir, &["--check", "ok.ttl", "todo.ttl"], None, 1);
    assert!(stderr(&o).contains("[warn] todo.ttl\n"), "{}", stderr(&o));
    let o = expect(dir, &["--check", "todo.ttl", "bad.ttl"], None, 2);
    assert!(
        stderr(&o).contains("bad.ttl:2:11: error: Turtle syntax error: "),
        "{}",
        stderr(&o)
    );
    let o = expect(dir, &["bad.trig"], None, 2);
    assert!(
        stderr(&o).starts_with("bad.trig:1:21: error: TriG syntax error: "),
        "{}",
        stderr(&o)
    );
    assert_eq!(read(dir, "bad.ttl"), BAD);
}

#[test]
fn the_nearest_config_file_applies() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "p/.sparklesfmt.toml", T1_GROUPS_TOML);
    write(dir, "p/t1.ttl", T1);
    write(dir, "t1.ttl", T1);
    let o = expect(dir, &["p/t1.ttl"], None, 0);
    assert_eq!(stdout(&o), T1_GROUPS);
    // flags override the file
    let o = expect(dir, &["--directive-style", "sparql", "p/t1.ttl"], None, 0);
    assert!(stdout(&o).starts_with("PREFIX rdf: "), "{}", stdout(&o));
    // stdin by name finds the same file
    let o = expect(dir, &["--stdin-filepath", "p/x.ttl"], Some(T1), 0);
    assert_eq!(stdout(&o), T1_GROUPS);
    // without it, the defaults
    let o = expect(dir, &["t1.ttl"], None, 0);
    assert!(stdout(&o).starts_with("PREFIX ex: "), "{}", stdout(&o));
}

#[test]
fn stdin_by_name_or_by_content() {
    let d = tempdir();
    let dir = d.path();
    let o = expect(dir, &["--stdin-filepath", "x.ttl"], Some(TURTLE), 0);
    assert_eq!(stdout(&o), TURTLE_OUT);
    let o = expect(dir, &["--stdin-filepath", "x.trig"], Some(TRIG), 0);
    assert_eq!(stdout(&o), TRIG_OUT);
    // sniffed: a Turtle statement, a TriG graph block
    let o = expect(dir, &[], Some(TURTLE), 0);
    assert_eq!(stdout(&o), TURTLE_OUT);
    let o = expect(dir, &[], Some(TRIG), 0);
    assert_eq!(stdout(&o), TRIG_OUT);
    let o = expect(dir, &["--language", "trig"], Some(TURTLE), 0);
    assert_eq!(stdout(&o), TURTLE_OUT);
    let o = expect(
        dir,
        &["--check", "--language", "turtle"],
        Some(TURTLE_OUT),
        0,
    );
    assert_eq!(stdout(&o), "");
}

/// With the test-only fault switch the printer drops a token: the graph check refuses
/// the output, `--write` leaves the file alone, and the endpoint answers 422.
#[test]
fn refused_output_is_never_written() {
    let d = tempdir();
    let dir = d.path();
    write(dir, "x.ttl", TURTLE);
    let fault = [("SPARKLES_FMT_FAULT", "drop-token")];
    for args in [&["--write", "x.ttl"][..], &["--check", "x.ttl"], &["x.ttl"]] {
        let o = fmt_in(dir, args, None, &fault);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains(
                "x.ttl: error: formatter refused its own output (graph differs); input left unchanged; please report"
            ),
            "{}",
            stderr(&o)
        );
        assert_eq!(stdout(&o), "");
        assert_eq!(read(dir, "x.ttl"), TURTLE);
    }

    let server = Server::start(dir, &fault);
    let (status, body) = server.post("/$/format", "text/turtle", TURTLE);
    assert_eq!(status, 422, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["code"], "unsafe-format", "{j}");
    assert!(
        j["error"].as_str().unwrap().contains("graph differs"),
        "{j}"
    );
}

/// `POST /$/format` on a real server: Turtle and TriG by media type and by name, syntax
/// errors with their position.
#[test]
fn the_endpoint_formats_turtle_and_trig() {
    let d = tempdir();
    let server = Server::start(d.path(), &[]);
    let (status, body) = server.post("/$/format", "text/turtle", TURTLE);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, TURTLE_OUT);
    let (status, body) = server.post("/$/format", "application/trig", TRIG);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, TRIG_OUT);
    let req = serde_json::json!({ "text": TURTLE, "language": "turtle", "options": { "directiveStyle": "turtle" } });
    let (status, body) = server.post("/$/format", "application/json", &req.to_string());
    assert_eq!(status, 200, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["language"], "turtle");
    assert!(
        j["text"]
            .as_str()
            .unwrap()
            .starts_with("@prefix ex: <http://example.org/> .\n"),
        "{j}"
    );
    let (status, body) = server.post("/$/format", "text/turtle", BAD);
    assert_eq!(status, 400, "{body}");
    let j: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(j["code"], "syntax", "{j}");
    assert_eq!(
        (j["line"].as_u64(), j["column"].as_u64()),
        (Some(2), Some(11)),
        "{j}"
    );
}

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
