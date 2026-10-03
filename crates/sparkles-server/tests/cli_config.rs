//! `sparkles config import fuseki`, `sparkles config check fuseki` and
//! `serve --fuseki-config` as the real binary
//! (spec G08), on Apache Jena's example configurations in `testsuite/fuseki/jena`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn jena() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/fuseki/jena")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn example(name: &str) -> String {
    jena().join("examples").join(name).display().to_string()
}

#[test]
fn import_writes_the_output_and_reports() {
    let d = tempfile::tempdir().unwrap();
    let o = run(
        d.path(),
        &[
            "config",
            "import",
            "fuseki",
            &example("config-tdb2.ttl"),
            "--out",
            "out",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("tdb2.tdbdump"), "{}", out(&o));
    let serve = std::fs::read_to_string(d.path().join("out/serve.sh")).unwrap();
    assert!(serve.contains("--loc dataset=db/dataset"), "{serve}");
    assert!(serve.contains("--union-default-graph"), "{serve}");
    let load = std::fs::read_to_string(d.path().join("out/load.sh")).unwrap();
    assert!(load.contains("tdb2.tdbdump --loc"), "{load}");
    assert!(
        load.contains("\"$SPARKLES\" load --loc db/dataset dataset.nq"),
        "{load}"
    );
    assert!(d.path().join("out/report.txt").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.path().join("out/serve.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    // a second run does not overwrite without --force
    let o = run(
        d.path(),
        &[
            "config",
            "import",
            "fuseki",
            &example("config-tdb2.ttl"),
            "--out",
            "out",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("--force"), "{}", err(&o));
}

#[test]
fn exit_statuses_and_formats() {
    let d = tempfile::tempdir().unwrap();
    // something important cannot be converted
    let o = run(
        d.path(),
        &[
            "config",
            "import",
            "fuseki",
            "--check",
            &example("tdb2-select-graphs.ttl"),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).contains("unsupported"), "{}", out(&o));
    // --check writes nothing
    assert!(!d.path().join("sparkles-config").exists());
    // no service at all
    let o = run(
        d.path(),
        &[
            "config",
            "import",
            "fuseki",
            "--check",
            &example("rdfs/vocabulary.ttl"),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let o = run(
        d.path(),
        &["config", "import", "fuseki", "--check", "missing.ttl"],
    );
    assert_eq!(o.status.code(), Some(2));
    // the report as JSON
    let o = run(
        d.path(),
        &[
            "config",
            "import",
            "fuseki",
            "--check",
            "--format",
            "json",
            &example("config-text-tdb2.ttl"),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "converted"
                && i["message"].as_str().unwrap().contains("rdfs:label")),
        "{v}"
    );
}

/// `config check` is `config import --check`: the same report and exit statuses, and
/// nothing written.
#[test]
fn check_is_import_check() {
    let d = tempfile::tempdir().unwrap();
    for (name, code) in [
        ("config-tdb2.ttl", 0),
        ("tdb2-select-graphs.ttl", 1),
        ("rdfs/vocabulary.ttl", 2),
    ] {
        let check = run(d.path(), &["config", "check", "fuseki", &example(name)]);
        let import = run(
            d.path(),
            &["config", "import", "fuseki", "--check", &example(name)],
        );
        assert_eq!(check.status.code(), Some(code), "{name}: {}", err(&check));
        assert_eq!(check.status.code(), import.status.code(), "{name}");
        assert_eq!(out(&check), out(&import), "{name}");
    }
    let o = run(
        d.path(),
        &[
            "config",
            "check",
            "fuseki",
            "--format",
            "json",
            &example("config-text-tdb2.ttl"),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap();
    assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
    // an unknown source is a usage error, and the help lists the sources
    let o = run(d.path(), &["config", "import", "jena", "config.ttl"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("fuseki"), "{}", err(&o));
}

#[test]
fn a_fuseki_base_directory_with_shiro() {
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("run");
    std::fs::create_dir_all(base.join("configuration")).unwrap();
    std::fs::copy(
        example("config-1-mem.ttl"),
        base.join("configuration/one.ttl"),
    )
    .unwrap();
    std::fs::copy(
        jena().join("testing/Shiro/shiro_userpassword.ini"),
        base.join("shiro.ini"),
    )
    .unwrap();
    let o = run(
        d.path(),
        &["config", "import", "fuseki", "run", "--out", "out"],
    );
    assert!(o.status.code() == Some(0), "{}{}", out(&o), err(&o));
    let report = out(&o);
    assert!(report.contains("shiro.ini"), "{report}");
    assert!(
        !report.contains("passwd1") && !err(&o).contains("passwd1"),
        "{report}"
    );
    let toml = std::fs::read_to_string(d.path().join("out/auth.toml")).unwrap();
    assert!(!toml.contains("passwd1"), "{toml}");
    #[cfg(feature = "auth")]
    {
        assert!(toml.contains("$argon2id$"), "{toml}");
        let o = run(d.path(), &["auth", "check", "--config", "out/auth.toml"]);
        assert!(o.status.success(), "{}{}\n{toml}", out(&o), err(&o));
    }
}

/// A free port of `range`, within 5580-5599, which no other test uses. Tests that run
/// at once use separate ranges.
fn port(range: std::ops::Range<u16>) -> u16 {
    range
        .clone()
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or_else(|| panic!("a free port in {range:?}"))
}

/// A GET on the server, returning the body.
fn get(port: u16, path: &str, accept: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nAccept: {accept}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).unwrap();
    body
}

/// A11: `serve --fuseki-config` loads `ja:data` into memory at each start.
#[test]
fn serve_from_a_fuseki_configuration() {
    let d = tempfile::tempdir().unwrap();
    std::fs::copy(example("data.trig"), d.path().join("data.trig")).unwrap();
    std::fs::write(
        d.path().join("config.ttl"),
        "PREFIX fuseki: <http://jena.apache.org/fuseki#>\n\
         PREFIX ja: <http://jena.hpl.hp.com/2005/11/Assembler#>\n\
         [] a fuseki:Server ; ja:context [ ja:cxtName \"arq:queryTimeout\" ; ja:cxtValue \"20000\" ] .\n\
         <#s> a fuseki:Service ; fuseki:name \"ds\" ;\n\
           fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name \"sparql\" ] ;\n\
           fuseki:endpoint [ fuseki:operation fuseki:update ] ;\n\
           fuseki:dataset [ a ja:MemoryDataset ; ja:data \"data.trig\" ] .\n",
    )
    .unwrap();
    let port = port(5580..5586);
    let mut child = Command::new(BIN)
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--data",
            "data",
            "--fuseki-config",
            "config.ttl",
            "--idle-release-ms",
            "0",
        ])
        .current_dir(d.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Ok(Some(status)) = child.try_wait() {
            let mut e = String::new();
            child.stderr.take().unwrap().read_to_string(&mut e).unwrap();
            panic!("the server exited with {status}: {e}");
        }
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let q = "/ds/sparql?query=SELECT%20(COUNT(*)%20AS%20%3Fn)%20%7B%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20UNION%20%7B%20GRAPH%20%3Fg%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20%7D%20%7D";
    let body = get(port, q, "text/csv");
    let _ = child.kill();
    let _ = child.wait();
    assert!(body.contains("\r\n\r\n"), "{body}");
    assert!(body.trim_end().ends_with('2'), "{body}");

    // a configuration with an unsupported part does not start
    let o = run(
        d.path(),
        &[
            "serve",
            "--port",
            &port.to_string(),
            "--data",
            "data2",
            "--fuseki-config",
            &example("tdb2-select-graphs.ttl"),
        ],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("no Sparkles equivalent"), "{}", err(&o));
}

/// The converted `serve.sh` starts a server: its flags parse, its settings files load.
#[cfg(all(feature = "text", feature = "geo"))]
#[test]
fn serve_sh_starts() {
    for (config, ds) in [
        (example("config-text-tdb2.ttl"), "dataset"),
        (
            jena()
                .join("testing/GeoAssembler/geo-config-ex.ttl")
                .display()
                .to_string(),
            "ds2",
        ),
    ] {
        let d = tempfile::tempdir().unwrap();
        let o = run(
            d.path(),
            &["config", "import", "fuseki", &config, "--out", "out"],
        );
        assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
        let port = port(5586..5600);
        let mut child = Command::new("sh")
            .arg(d.path().join("out/serve.sh"))
            .args(["--port", &port.to_string(), "--idle-release-ms", "0"])
            .env("SPARKLES", BIN)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            if let Ok(Some(status)) = child.try_wait() {
                let mut e = String::new();
                child.stderr.take().unwrap().read_to_string(&mut e).unwrap();
                panic!("{config}: the server exited with {status}: {e}");
            }
            assert!(
                t0.elapsed() < Duration::from_secs(120),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let body = get(port, &format!("/$/datasets/{ds}"), "application/json");
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            body.starts_with("HTTP/1.0 200") || body.starts_with("HTTP/1.1 200"),
            "{body}"
        );
        assert!(
            body.contains("\"text\":{") || body.contains("\"geo\":{"),
            "{body}"
        );
        assert!(d.path().join("out/db").join(ds).is_dir(), "{config}");
    }
}
