//! The `sparkles` command line against a real server with authentication: offline
//! helpers, `auth login` (device and browser), status, tokens, logout, and the remote
//! `query`, `update` and `load`.
#![cfg(feature = "auth")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn sparkles(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env_remove("SPARKLES_SERVER")
        .env_remove("SPARKLES_TOKEN")
        .env_remove("BROWSER")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("SSH_CONNECTION")
        .env_remove("SSH_TTY");
    c
}

fn run(c: &mut Command) -> Output {
    c.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn hash_password(home: &Path, pw: &str) -> String {
    let mut child = sparkles(home)
        .args(["auth", "hash"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{pw}").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    stdout(&o).trim().to_string()
}

/// A server on 127.0.0.1 with datasets `wiki` and `secret`; alice (password
/// `alice-pw`) administers everything.
struct Server {
    child: Child,
    url: String,
    home: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Server {
    let home = tempfile::tempdir().unwrap();
    let hash = hash_password(home.path(), "alice-pw");
    let cfg = home.path().join("auth.toml");
    std::fs::write(
        &cfg,
        format!(
            "version = 1\n[[users]]\nname = \"alice\"\npassword = \"{hash}\"\nserver = [\"server-admin\"]\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let port = free_port();
    let child = Command::new(BIN)
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--mem",
            "wiki",
            "--mem",
            "secret",
            "--idle-release-ms",
            "0",
        ])
        .arg("--data")
        .arg(home.path().join("data"))
        .arg("--auth-config")
        .arg(&cfg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let t0 = Instant::now();
    while reqwest::blocking::get(format!("{url}/$/ping")).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { child, url, home }
}

/// A browser session of alice: cookie header and CSRF token.
fn alice_session(url: &str) -> (String, String) {
    let c = reqwest::blocking::Client::new();
    let r = c
        .post(format!("{url}/$/auth/login"))
        .header("content-type", "application/json")
        .body(r#"{"user":"alice","password":"alice-pw"}"#)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    let set = r.headers()["set-cookie"].to_str().unwrap();
    let cookie = set.split(';').next().unwrap().to_string();
    let who: serde_json::Value = serde_json::from_slice(
        &c.get(format!("{url}/$/whoami"))
            .header("cookie", &cookie)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
    )
    .unwrap();
    (cookie, who["csrfToken"].as_str().unwrap().to_string())
}

fn approve_device(url: &str, code: &str) {
    let (cookie, csrf) = alice_session(url);
    let r = reqwest::blocking::Client::new()
        .post(format!("{url}/$/auth/device/{code}/approve"))
        .header("cookie", cookie)
        .header("x-sparkles-csrf", csrf)
        .header("content-type", "application/json")
        .body(r#"{"name":"laptop"}"#)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "{:?}", r.text());
}

fn credentials(home: &Path) -> PathBuf {
    home.join("config/sparkles/credentials.toml")
}

#[test]
fn offline_commands() {
    let home = tempfile::tempdir().unwrap();
    let h = hash_password(home.path(), "pw");
    let re = regex::Regex::new(
        r"^\$argon2id\$v=19\$m=19456,t=2,p=1\$[A-Za-z0-9+/]{22}\$[A-Za-z0-9+/]{43}$",
    )
    .unwrap();
    assert!(re.is_match(&h), "{h}");

    let o = run(sparkles(home.path()).args(["auth", "gen-token", "--name", "x"]));
    assert!(o.status.success());
    let token = stdout(&o).trim().to_string();
    assert!(
        regex::Regex::new(r"^spk_[A-Za-z0-9_-]{43}$")
            .unwrap()
            .is_match(&token)
    );
    let mut child = sparkles(home.path())
        .args(["auth", "hash", "--token"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{token}").unwrap();
    let hashed = stdout(&child.wait_with_output().unwrap())
        .trim()
        .to_string();
    assert!(
        stderr(&o).contains(&format!("hash = \"{hashed}\"")),
        "{}",
        stderr(&o)
    );

    let bad = home.path().join("bad.toml");
    std::fs::write(&bad, "version = 1\n[[users]]\nname = \"a\"\ndataset = {}\n").unwrap();
    let o = run(sparkles(home.path())
        .args(["auth", "check", "--config"])
        .arg(&bad));
    assert_eq!(o.status.code(), Some(1));
    let e = stderr(&o);
    assert!(
        e.contains("dataset") && e.contains("line 4") && e.contains("column"),
        "{e}"
    );

    let o = run(sparkles(home.path())
        .args(["serve", "--port", "1", "--auth-config"])
        .arg(home.path().join("missing.toml"))
        .arg("--data")
        .arg(home.path().join("data")));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("missing.toml"), "{}", stderr(&o));
}

#[test]
fn device_login_status_tokens_and_logout() {
    let s = start();
    let mut child = sparkles(s.home.path())
        .args(["auth", "login", "--device", "--server", &s.url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let err = BufReader::new(child.stderr.take().unwrap());
    let re = regex::Regex::new(r"confirm the code ([A-Z2-9]{4}-[A-Z2-9]{4})").unwrap();
    let mut code = None;
    for line in err.lines() {
        let line = line.unwrap();
        if let Some(c) = re.captures(&line) {
            code = Some(c[1].to_string());
            break;
        }
    }
    approve_device(&s.url, &code.expect("no user code printed"));
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains(&format!("Logged in to {} as user:alice", s.url)),
        "{}",
        stdout(&o)
    );
    let creds = credentials(s.home.path());
    let text = std::fs::read_to_string(&creds).unwrap();
    assert!(
        text.contains("token = \"spk_") && text.contains("token_id = \"tok_"),
        "{text}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&creds), 0o600);
        assert_eq!(mode(creds.parent().unwrap()), 0o700);
    }

    let st = run(sparkles(s.home.path()).args(["auth", "status"]));
    let out = stdout(&st);
    assert!(
        out.contains("principal  user:alice") && out.contains("token      tok_"),
        "{out}"
    );
    assert!(out.contains("(default)"), "{out}");

    // remote client
    let q = run(sparkles(s.home.path()).args([
        "query",
        "--server",
        &s.url,
        "--dataset",
        "wiki",
        "--results",
        "json",
        "ASK { ?s ?p ?o }",
    ]));
    assert!(q.status.success(), "{}", stderr(&q));
    assert!(stdout(&q).contains("\"boolean\":false"), "{}", stdout(&q));
    let u = run(sparkles(s.home.path()).args([
        "update",
        "--server",
        &s.url,
        "--dataset",
        "wiki",
        "INSERT DATA { <urn:a> <urn:b> <urn:c> }",
    ]));
    assert!(u.status.success(), "{}", stderr(&u));
    let q = run(sparkles(s.home.path()).args([
        "query",
        "--server",
        &s.url,
        "--dataset",
        "wiki",
        "--results",
        "json",
        "ASK { ?s ?p ?o }",
    ]));
    assert!(stdout(&q).contains("\"boolean\":true"), "{}", stdout(&q));
    let gz = s.home.path().join("data.ttl.gz");
    let mut enc = flate2::write::GzEncoder::new(
        std::fs::File::create(&gz).unwrap(),
        flate2::Compression::default(),
    );
    enc.write_all(b"<urn:x> <urn:y> 1, 2, 3 .\n").unwrap();
    enc.finish().unwrap();
    let l = run(sparkles(s.home.path())
        .args(["load", "--server", &s.url, "--dataset", "wiki"])
        .arg(&gz));
    assert!(l.status.success(), "{}", stderr(&l));
    assert!(stdout(&l).contains("loaded 3 quads"), "{}", stdout(&l));
    let bad = run(sparkles(s.home.path()).env("SPARKLES_TOKEN", "bad").args([
        "query",
        "--server",
        &s.url,
        "--dataset",
        "wiki",
        "ASK {}",
    ]));
    assert_eq!(bad.status.code(), Some(1));
    assert!(
        stderr(&bad).contains(&format!(
            "not logged in to {} (run: sparkles auth login --server {})",
            s.url, s.url
        )),
        "{}",
        stderr(&bad)
    );
    let nope = run(sparkles(s.home.path()).args([
        "query",
        "--server",
        &s.url,
        "--dataset",
        "nope",
        "ASK {}",
    ]));
    assert_eq!(nope.status.code(), Some(1));
    assert!(stderr(&nope).contains("no such dataset: /nope (or no access)"));
    let plain = run(sparkles(s.home.path()).args([
        "query",
        "--server",
        "http://example.org",
        "--dataset",
        "wiki",
        "ASK {}",
    ]));
    assert_eq!(plain.status.code(), Some(1));
    assert!(
        stderr(&plain).contains("--insecure-http"),
        "{}",
        stderr(&plain)
    );

    // tokens
    let c = run(sparkles(s.home.path()).args([
        "auth",
        "token",
        "create",
        "--name",
        "ci",
        "--dataset",
        "wiki=read",
        "--expires",
        "7d",
    ]));
    assert!(c.status.success(), "{}", stderr(&c));
    let ci = stdout(&c).trim().to_string();
    assert!(ci.starts_with("spk_") && !ci.contains('\n'));
    let id = stderr(&c).split_whitespace().nth(1).unwrap().to_string();
    let l = run(sparkles(s.home.path()).args(["auth", "token", "list"]));
    assert!(
        stdout(&l).contains(&id) && stdout(&l).contains("wiki=read"),
        "{}",
        stdout(&l)
    );
    let r = run(sparkles(s.home.path()).args(["auth", "token", "revoke", &id]));
    assert!(r.status.success(), "{}", stderr(&r));
    let gone = run(sparkles(s.home.path()).env("SPARKLES_TOKEN", &ci).args([
        "query",
        "--server",
        &s.url,
        "--dataset",
        "wiki",
        "ASK {}",
    ]));
    assert_eq!(gone.status.code(), Some(1));

    // logout revokes the stored token
    let token = text
        .lines()
        .find_map(|l| l.strip_prefix("token = \""))
        .unwrap()
        .trim_end_matches('"')
        .to_string();
    let lo = run(sparkles(s.home.path()).args(["auth", "logout"]));
    assert!(lo.status.success(), "{}", stderr(&lo));
    assert!(!std::fs::read_to_string(&creds).unwrap().contains("spk_"));
    let r = reqwest::blocking::Client::new()
        .get(format!("{}/$/whoami", s.url))
        .bearer_auth(&token)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn browser_login() {
    let s = start();
    let url_file = s.home.path().join("browser-url");
    let script = s.home.path().join("browser.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", url_file.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let child = sparkles(s.home.path())
        .env("BROWSER", &script)
        .args(["auth", "login", "--web", "--server", &s.url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let opened = loop {
        if let Ok(u) = std::fs::read_to_string(&url_file)
            && !u.is_empty()
        {
            break u;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "no browser was opened"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        opened.starts_with(&format!("{}/ui/cli/authorize?", s.url)),
        "{opened}"
    );
    let q: std::collections::HashMap<String, String> =
        form_urlencoded::parse(opened.split_once('?').unwrap().1.as_bytes())
            .into_owned()
            .collect();
    // what the approval page does
    let (cookie, csrf) = alice_session(&s.url);
    let body = serde_json::json!({
        "port": q["port"].parse::<u32>().unwrap(),
        "state": q["state"],
        "codeChallenge": q["code_challenge"],
        "label": q["label"],
        "hostname": q["hostname"],
        "name": "laptop",
    });
    let r = reqwest::blocking::Client::new()
        .post(format!("{}/$/auth/cli/authorize", s.url))
        .header("cookie", cookie)
        .header("x-sparkles-csrf", csrf)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let redirect: serde_json::Value = serde_json::from_slice(&r.bytes().unwrap()).unwrap();
    let page = reqwest::blocking::get(redirect["redirect"].as_str().unwrap())
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains("authorized"), "{page}");
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("as user:alice"), "{}", stdout(&o));
    assert!(
        std::fs::read_to_string(credentials(s.home.path()))
            .unwrap()
            .contains("token = \"spk_")
    );

    // over SSH without --web the device flow is chosen
    let mut child = sparkles(s.home.path())
        .env("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
        .env("DISPLAY", ":0")
        .args(["auth", "login", "--server", &s.url])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    while first.trim().is_empty() {
        first.clear();
        err.read_line(&mut first).unwrap();
    }
    assert!(first.contains("To authorize this device"), "{first}");
    let _ = child.kill();
    let _ = child.wait();
}
