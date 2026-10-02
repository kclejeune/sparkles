//! `sparkles vector`: create, list, status, rebuild and drop vector indexes, run as the
//! real binary against a local database and against a server.

use serde_json::Value as J;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

#[track_caller]
fn expect(args: &[&str], code: i32) -> Output {
    let o = Command::new(BIN)
        .args(args)
        .env_remove("SPARKLES_SERVER")
        .env_remove("SPARKLES_TOKEN")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn fixture(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let x = i as f32 / 7.0;
        s += &format!(
            "<urn:v{i}> <urn:emb> \"[{}, {}, 0.5]\"^^<urn:x-sparkles:vector> .\n",
            x.sin(),
            x.cos()
        );
    }
    s += "<urn:w> <urn:emb> \"[1, 2]\"^^<urn:x-sparkles:vector> .\n";
    s
}

#[test]
fn vector_index_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.nt");
    std::fs::write(&data, fixture(500)).unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    let o = expect(&["vector", "list", "--loc", &db], 0);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no vector indexes"));
    let o = expect(
        &[
            "vector",
            "create",
            "--loc",
            &db,
            "--name",
            "emb",
            "--predicate",
            "urn:emb",
            "--dim",
            "3",
            "--m",
            "8",
            "--ef-search",
            "40",
        ],
        0,
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("created") && err.contains("500 rows"), "{err}");
    assert!(Path::new(&db).join("vector.json").exists());
    // a fresh process maps the index from its file
    let o = expect(&["vector", "status", "--loc", &db, "--name", "emb"], 0);
    let s: J = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["state"], "ready");
    assert_eq!(s["rows"], 500);
    assert_eq!(s["skipped"]["wrongDimension"], 1);
    assert_eq!(s["files"]["opened"], true);
    assert_eq!(s["hnsw"]["efSearch"], 40);
    let o = expect(&["vector", "list", "--loc", &db], 0);
    let table = String::from_utf8_lossy(&o.stdout);
    assert!(table.contains("emb") && table.contains("M=8"), "{table}");
    // errors: an unknown index, a bad metric, a second index on the predicate
    expect(&["vector", "rebuild", "--loc", &db, "--name", "nope"], 1);
    let o = expect(
        &[
            "vector",
            "create",
            "--loc",
            &db,
            "--name",
            "x",
            "--predicate",
            "urn:emb",
            "--dim",
            "3",
            "--metric",
            "hamming",
        ],
        1,
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("--metric hamming"));
    let o = expect(
        &[
            "vector",
            "create",
            "--loc",
            &db,
            "--name",
            "x",
            "--predicate",
            "urn:emb",
            "--dim",
            "3",
        ],
        1,
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("already indexed"));
    let o = expect(&["vector", "rebuild", "--loc", &db, "--name", "emb"], 0);
    assert!(String::from_utf8_lossy(&o.stderr).contains("rebuilt"));
    // queries use it
    let o = expect(
        &[
            "query",
            "--loc",
            &db,
            "--results",
            "tsv",
            "SELECT ?s { ?s <urn:x-sparkles:vectorSearch> (<urn:emb> <urn:v3> 1) }",
        ],
        0,
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("<urn:v3>"));
    // check validates vector.json and the index file
    let check = |code: i32| {
        let o = expect(&["check", "--loc", &db, "--format", "json"], code);
        let report: J = serde_json::from_slice(&o.stdout).unwrap();
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "vector")
            .cloned()
            .unwrap_or_else(|| panic!("{report}"))
    };
    assert_eq!(check(0)["status"], "ok");
    let cfg = Path::new(&db).join("vector.json");
    let saved = std::fs::read(&cfg).unwrap();
    std::fs::write(
        &cfg,
        br#"{"indexes":{"x":{"predicate":"urn:emb","dimension":0}}}"#,
    )
    .unwrap();
    assert!(check(1).to_string().contains("dimension"));
    std::fs::write(&cfg, saved).unwrap();
    // clone copies the configuration
    let copy = dir.path().join("copy");
    expect(&["clone", "--loc", &db, "--to", copy.to_str().unwrap()], 0);
    assert!(copy.join("vector.json").exists());
    expect(&["vector", "drop", "--loc", &db, "--name", "emb"], 0);
    assert!(!Path::new(&db).join("vector.json").exists());
    let o = expect(&["vector", "status", "--loc", &db], 0);
    assert_eq!(
        serde_json::from_slice::<J>(&o.stdout).unwrap(),
        serde_json::json!([])
    );
}

/// A free port in 4800–4819.
#[cfg(feature = "auth")]
fn port() -> u16 {
    (4800..4820)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("a free port in 4800-4819")
}

#[cfg(feature = "auth")]
#[test]
fn vector_index_on_a_server() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let port = port();
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = Kill(
        Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--mem", "ds", "--data"])
            .arg(dir.path().join("data"))
            .env_remove("SPARKLES_SERVER")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let t0 = Instant::now();
    while reqwest::blocking::get(format!("{url}/$/ping")).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let remote = |args: &[&str], code: i32| {
        let mut a: Vec<&str> = args.to_vec();
        a.extend(["--server", &url, "--dataset", "ds"]);
        expect(&a, code)
    };
    expect(
        &[
            "update",
            "--server",
            &url,
            "--dataset",
            "ds",
            "INSERT DATA { <urn:a> <urn:emb> \"[1, 0, 0]\"^^<urn:x-sparkles:vector> . <urn:b> <urn:emb> \"[0, 1, 0]\"^^<urn:x-sparkles:vector> }",
        ],
        0,
    );
    let o = remote(
        &[
            "vector",
            "create",
            "--name",
            "emb",
            "--predicate",
            "urn:emb",
            "--dim",
            "3",
        ],
        0,
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("created"));
    let o = remote(&["vector", "status", "--name", "emb"], 0);
    let s: J = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["state"], "ready");
    // the inserted vectors are served from the overlay
    assert_eq!(s["overlay"]["inserts"], 2);
    let o = remote(&["vector", "list"], 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("urn:emb"));
    remote(&["vector", "rebuild", "--name", "emb"], 0);
    remote(&["vector", "drop", "--name", "emb"], 0);
    remote(&["vector", "drop", "--name", "emb"], 1);
    let o = remote(&["vector", "status"], 0);
    assert_eq!(
        serde_json::from_slice::<J>(&o.stdout).unwrap(),
        serde_json::json!([])
    );
}
