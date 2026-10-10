//! `sparkles models` against a mock Hub on a local port: pull with verification, a second
//! pull that makes no request, list, verify (and its failure after corruption), rm, and
//! the refusal of an unpinned revision without `--allow-unpinned`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sparkles_modelstore::{Digest, git_blob_reader, sha256_reader};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const REV: &str = "89abcdef0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct Hub {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: AtomicUsize,
}

fn serve(port: u16, hub: Arc<Hub>) -> u16 {
    let l = TcpListener::bind(("127.0.0.1", port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let h = hub.clone();
            std::thread::spawn(move || handle(s, &h));
        }
    });
    port
}

fn handle(s: TcpStream, hub: &Hub) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split(' ').nth(1).unwrap_or("").to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
    }
    hub.requests.fetch_add(1, Ordering::SeqCst);
    let mut w = s;
    let body = hub.files.lock().unwrap().get(&path).cloned();
    match body {
        Some(b) => {
            let _ = write!(
                w,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                b.len()
            );
            let _ = w.write_all(&b);
        }
        None => {
            let _ = w.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
}

/// The repo `org/mini` at `REV`, also listed under the branch `main`, with a config
/// (Git blob id), weights (LFS SHA-256) and a file outside the default selection.
fn hub(port: u16) -> (Arc<Hub>, String) {
    let config = br#"{"model_type":"bert"}"#.to_vec();
    let weights: Vec<u8> = (0..50_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let wsha =
        Digest::Sha256(sha256_reader(&mut &weights[..]).unwrap()).to_tagged()[7..].to_string();
    let cblob =
        Digest::GitBlobSha1(git_blob_reader(&mut &config[..], config.len() as u64).unwrap())
            .to_tagged()[9..]
            .to_string();
    let listing = format!(
        r#"{{"id":"org/mini","sha":"{REV}","siblings":[
            {{"rfilename":"config.json","size":{},"blobId":"{cblob}"}},
            {{"rfilename":"model.safetensors","size":{},"blobId":"x","lfs":{{"sha256":"{wsha}","size":{}}}}},
            {{"rfilename":"onnx/model.onnx","size":3,"blobId":"y"}}
        ]}}"#,
        config.len(),
        weights.len(),
        weights.len()
    );
    let h = Arc::new(Hub::default());
    {
        let mut f = h.files.lock().unwrap();
        for rev in [REV, "main"] {
            f.insert(
                format!("/api/models/org/mini/revision/{rev}?blobs=true"),
                listing.clone().into_bytes(),
            );
        }
        f.insert(format!("/org/mini/resolve/{REV}/config.json"), config);
        f.insert(
            format!("/org/mini/resolve/{REV}/model.safetensors"),
            weights,
        );
    }
    let port = serve(port, h.clone());
    (h, format!("http://127.0.0.1:{port}"))
}

fn models(dir: &Path, endpoint: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("models")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .env("SPARKLES_HUB_ENDPOINT", endpoint)
        .env_remove("HF_TOKEN")
        .env_remove("SPARKLES_MODELS_DIR")
        .output()
        .unwrap()
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn pull_list_verify_rm() {
    let (h, url) = hub(48010);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("models");
    let reference = format!("org/mini@{REV}");

    let out = ok(&models(&dir, &url, &["pull", &reference]));
    assert!(out.contains("pulled"), "{out}");
    let snap = dir.join("org/mini").join(REV);
    assert!(snap.join("config.json").is_file());
    assert!(snap.join("model.safetensors").is_file());
    assert!(
        !snap.join("onnx").exists(),
        "onnx is outside the default selection"
    );
    assert!(snap.join("sparkles-manifest.json").is_file());
    let first = h.requests.load(Ordering::SeqCst);
    assert_eq!(first, 3, "one listing and two files");

    // the snapshot is present, so no request
    let out = ok(&models(&dir, &url, &["pull", &reference, "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["pulled"], false);
    assert_eq!(h.requests.load(Ordering::SeqCst), first);

    let out = ok(&models(&dir, &url, &["list", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["repo"], "org/mini");
    assert_eq!(v[0]["revision"], REV);
    assert_eq!(v[0]["bytes"], 50_000 + 21);
    let files = v[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|f| f["sha256"].as_str().unwrap().len() == 64)
    );

    let out = ok(&models(&dir, &url, &["verify", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["ok"], true);

    // corruption fails verification with a non-zero exit
    std::fs::write(snap.join("config.json"), br#"{"model_type":"BERT"}"#).unwrap();
    let o = models(&dir, &url, &["verify", "--json"]);
    assert!(!o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v[0]["ok"], false);
    assert!(v[0]["error"].as_str().unwrap().contains("config.json"));

    ok(&models(&dir, &url, &["rm", &reference]));
    assert!(!snap.exists());
    let o = models(&dir, &url, &["rm", &reference]);
    assert!(!o.status.success(), "removing a missing snapshot fails");
    let out = ok(&models(&dir, &url, &["list", "--json"]));
    assert_eq!(out.trim(), "[]");
}

#[test]
fn unpinned_and_manifest() {
    let (h, url) = hub(48011);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("models");

    for r in ["org/mini", "org/mini@main"] {
        let o = models(&dir, &url, &["pull", r]);
        assert!(!o.status.success(), "{r} is not pinned");
        assert!(String::from_utf8_lossy(&o.stderr).contains("allow-unpinned"));
    }
    assert_eq!(h.requests.load(Ordering::SeqCst), 0);

    let out = ok(&models(
        &dir,
        &url,
        &["pull", "org/mini@main", "--allow-unpinned", "--json"],
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["revision"], REV, "the branch resolves to its commit");
    assert!(
        dir.join("org/mini")
            .join(REV)
            .join("sparkles-manifest.json")
            .is_file()
    );

    // a manifest naming a file subset; a missing file fails the whole command
    let m = tmp.path().join("m.json");
    std::fs::write(
        &m,
        format!(
            r#"{{"models":[{{"repo":"org/mini","revision":"{REV}","files":["config.json"]}}]}}"#
        ),
    )
    .unwrap();
    let other = tmp.path().join("other");
    ok(&models(
        &other,
        &url,
        &["pull", "--manifest", m.to_str().unwrap()],
    ));
    let snap = other.join("org/mini").join(REV);
    assert!(snap.join("config.json").is_file());
    assert!(!snap.join("model.safetensors").exists());

    std::fs::write(
        &m,
        format!(
            r#"{{"models":[{{"repo":"org/mini","revision":"{REV}","files":["absent.bin"]}}]}}"#
        ),
    )
    .unwrap();
    let o = models(
        &tmp.path().join("third"),
        &url,
        &["pull", "--manifest", m.to_str().unwrap()],
    );
    assert!(!o.status.success());

    // nothing to pull is an error
    assert!(!models(&dir, &url, &["pull"]).status.success());
}
